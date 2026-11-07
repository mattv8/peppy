//! Bounded MCP tool catalogue and dispatcher.
//!
//! This module deliberately returns MCP result objects rather than JSON-RPC
//! envelopes; the public server owns protocol framing.
use std::{
    fs,
    io::{Cursor, Read},
    str::FromStr,
    sync::{Arc, LazyLock},
};

use base64::{Engine as _, engine::general_purpose::STANDARD};
use image::{ImageReader, Limits};
use peppy_client_core::{
    AttachmentId, Client, ConversationId, Direction, GatewayRoute, McpCommand, MessageId,
};
use peppy_desktop_api::{GatewayAvailability, GatewayView};
use peppy_hosted_api::mcp::{
    MCP_TOOL_SCOPES, McpScope, McpWorkerCallRequest, mcp_scopes_allow, mcp_tool_scopes,
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use thiserror::Error;
use uuid::Uuid;

use crate::Runtime;

const MAX_CONVERSATIONS: usize = 100;
const MAX_MESSAGES: usize = 100;
const MAX_ATTACHMENT_DESCRIPTORS: usize = 20;
const MAX_ROUTE_CHOICES: usize = 100;
const MAX_ATTACHMENT_BYTES: u64 = 2 * 1024 * 1024;
const MAX_IMAGE_DIMENSION: u32 = 4096;
const MAX_IMAGE_DECODE_ALLOCATION: u64 = 64 * 1024 * 1024;
const MAX_TOOL_RESULT_BYTES: usize = 3 * 1024 * 1024;
static IMAGE_DECODE_SLOTS: LazyLock<Arc<tokio::sync::Semaphore>> =
    LazyLock::new(|| Arc::new(tokio::sync::Semaphore::new(2)));

#[derive(Debug, Error)]
pub enum ToolError {
    #[error("invalid tools/call request")]
    InvalidRequest,
    #[error("scope is not granted")]
    Forbidden,
    #[error("runtime identity does not match request")]
    IdentityMismatch,
}

pub fn list_tools(scopes: &[McpScope]) -> Value {
    let mut tools = Vec::new();
    for entry in MCP_TOOL_SCOPES {
        if mcp_scopes_allow(scopes, entry.required_scopes) {
            let tool = match entry.name {
                "list_conversations" => tool(
                    "list_conversations",
                    "List the vault's recent conversations.",
                    json!({"type":"object","properties":{"limit":{"type":"integer","minimum":1,"maximum":100}},"additionalProperties":false}),
                ),
                "get_messages" => tool(
                    "get_messages",
                    "Get a bounded chronological page of messages.",
                    json!({"type":"object","required":["conversation_id"],"properties":{"conversation_id":{"type":"string","format":"uuid"},"before":{"type":"string","format":"uuid"},"limit":{"type":"integer","minimum":1,"maximum":100}},"additionalProperties":false}),
                ),
                "get_attachment" => tool(
                    "get_attachment",
                    "Return a bounded local raster image as inline base64.",
                    json!({"type":"object","required":["attachment_id"],"properties":{"attachment_id":{"type":"string","format":"uuid"}},"additionalProperties":false}),
                ),
                "list_contacts" => tool(
                    "list_contacts",
                    "List sanitized contacts from a local contact book.",
                    json!({"type":"object","properties":{"book_id":{"type":"string"},"query":{"type":"string","maxLength":1024},"limit":{"type":"integer","minimum":1,"maximum":100}},"additionalProperties":false}),
                ),
                "send_message" => tool(
                    "send_message",
                    "Queue a text message; queued does not mean delivered.",
                    write_schema(false),
                ),
                "send_reaction_text" => tool(
                    "send_reaction_text",
                    "Queue literal reaction text as an ordinary message.",
                    write_schema(true),
                ),
                "send_image" => tool(
                    "send_image",
                    "Queue an inline PNG, JPEG, or WebP image (at most 2 MiB encoded, 4096 pixels per side, and 64 MiB decoded) as MMS; queued does not mean delivered.",
                    image_schema(),
                ),
                _ => continue,
            };
            tools.push(tool);
        }
    }
    json!({"tools": tools})
}

pub async fn call_tool(
    runtime: &Runtime,
    request: &McpWorkerCallRequest,
) -> Result<Value, ToolError> {
    if runtime.vault_id().0 != request.vault_id || runtime.device_id().0 != request.device_id {
        return Err(ToolError::IdentityMismatch);
    }
    let params = request
        .request
        .get("params")
        .ok_or(ToolError::InvalidRequest)?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or(ToolError::InvalidRequest)?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let required = required_scopes(name).ok_or(ToolError::InvalidRequest)?;
    if !mcp_scopes_allow(&request.scopes, required) {
        return Ok(error_result(
            "forbidden",
            "The grant does not permit this tool.",
        ));
    }
    let result = match name {
        "list_conversations" => list_conversations(runtime.client(), &arguments),
        "get_messages" => get_messages(runtime.client(), &arguments),
        "get_attachment" => get_attachment(runtime.client(), &arguments).await,
        "list_contacts" => list_contacts(runtime.client(), &arguments),
        "send_message" => send_message(runtime, request, &arguments).await,
        "send_image" => send_image(runtime, request, &arguments).await,
        "send_reaction_text" => send_reaction_text(runtime, request, &arguments).await,
        _ => Err(("unknown_tool", "Unknown tool.")),
    };
    Ok(match result {
        Ok(value) if value.get("content").is_some() => value,
        Ok(value) => success(value),
        Err((code, message)) => error_result(code, message),
    })
}

fn required_scopes(name: &str) -> Option<&'static [McpScope]> {
    mcp_tool_scopes(name)
}

fn list_conversations(
    client: &Client,
    args: &Value,
) -> Result<Value, (&'static str, &'static str)> {
    let limit = bounded_limit(args, MAX_CONVERSATIONS)?;
    let conversations = client.list_conversations().map_err(core_error)?;
    Ok(
        json!({"conversations": conversations.into_iter().take(limit).map(|item| json!({"conversation_id":item.conversation_id,"unread_count":item.unread_count,"pinned":item.pinned,"folder":format!("{:?}", item.folder).to_lowercase()})).collect::<Vec<_>>() }),
    )
}

fn get_messages(client: &Client, args: &Value) -> Result<Value, (&'static str, &'static str)> {
    let conversation_id = id(args, "conversation_id")
        .map(ConversationId)
        .map_err(|_| ("invalid_arguments", "conversation_id must be a UUID."))?;
    let before = args
        .get("before")
        .map(|_| id(args, "before").map(MessageId))
        .transpose()
        .map_err(|_| ("invalid_arguments", "before must be a UUID."))?;
    let page = client
        .messages_page(conversation_id, before, bounded_limit(args, MAX_MESSAGES)?)
        .map_err(core_error)?;
    let page_has_more = page.has_more;
    let page_message_count = page.messages.len();
    let mut envelope_bytes = serialized_success_len(&json!({"messages":[],"has_more":false}));
    let mut newest_first = Vec::new();
    for message in page.messages.into_iter().rev() {
        let message = json!({"message_id":message.payload.record.message_id,"conversation_id":message.payload.record.conversation_id,"body":message.payload.body,"attachments":attachment_descriptors(client, message.payload.record.attachments.iter().map(|item| item.attachment_id)),"recipients":message.payload.recipients,"direction":match message.payload.direction { Direction::Incoming => "incoming", Direction::Outgoing => "outgoing" },"transport":format!("{:?}", message.payload.transport).to_lowercase(),"send_state":message.send_state.map(|state| format!("{:?}",state).to_lowercase())});
        let contribution = escaped_json_string_content_len(&message);
        let separator = usize::from(!newest_first.is_empty());
        if envelope_bytes + separator + contribution > MAX_TOOL_RESULT_BYTES {
            break;
        }
        envelope_bytes += separator + contribution;
        newest_first.push(message);
    }
    let has_more = page_has_more || newest_first.len() < page_message_count;
    newest_first.reverse();
    let messages = newest_first;
    Ok(json!({"messages":messages,"has_more":has_more}))
}

fn attachment_descriptors(client: &Client, ids: impl Iterator<Item = AttachmentId>) -> Vec<Value> {
    ids.take(MAX_ATTACHMENT_DESCRIPTORS).map(|id| {
        let info = client.attachment_info(id).ok();
        json!({"attachment_id":id,
            "mime":info.as_ref().map(|item| item.media_type.chars().take(256).collect::<String>()),
            "name":info.as_ref().map(|item| item.display_name.chars().take(256).collect::<String>()),
            "size":info.as_ref().map(|item| item.plaintext_bytes)})
    }).collect()
}

async fn get_attachment(
    client: &Client,
    args: &Value,
) -> Result<Value, (&'static str, &'static str)> {
    let attachment_id = id(args, "attachment_id")
        .map(AttachmentId)
        .map_err(|_| ("invalid_arguments", "attachment_id must be a UUID."))?;
    let file = client
        .open_native_plaintext(attachment_id)
        .map_err(core_error)?;
    let mut bytes = Vec::new();
    fs::File::open(file.path())
        .and_then(|file| file.take(MAX_ATTACHMENT_BYTES + 1).read_to_end(&mut bytes))
        .map_err(|_| {
            (
                "attachment_unavailable",
                "The attachment is not locally available.",
            )
        })?;
    attachment_result(&bytes).await
}

async fn attachment_result(bytes: &[u8]) -> Result<Value, (&'static str, &'static str)> {
    let mime = validate_raster_image(bytes).await.map_err(|_| {
        (
            "unsupported_attachment",
            "The attachment must be a valid bounded PNG, JPEG, or WebP.",
        )
    })?;
    Ok(
        json!({"content":[{"type":"image","mimeType":mime,"data":STANDARD.encode(bytes)}],"isError":false}),
    )
}

fn list_contacts(client: &Client, args: &Value) -> Result<Value, (&'static str, &'static str)> {
    let Some(book_id) = args.get("book_id").and_then(Value::as_str) else {
        let raw = client.list_contact_books_json().map_err(core_error)?;
        let value: Value = serde_json::from_str(&raw)
            .map_err(|_| ("core_error", "Local contact data is unavailable."))?;
        return Ok(json!({"books":value.get("books").cloned().unwrap_or_else(|| json!([]))}));
    };
    let query = args.get("query").and_then(Value::as_str).unwrap_or("");
    if query.len() > 1024 {
        return Err(("invalid_arguments", "query is too long."));
    }
    let raw = client.contact_book_view(&json!({"schema_version":1,"book_id":book_id,"search":query,"limit":bounded_limit(args, 100)?,"offset":0}).to_string()).map_err(core_error)?;
    let value: Value = serde_json::from_str(&raw)
        .map_err(|_| ("core_error", "Local contact data is unavailable."))?;
    Ok(json!({"contacts":value.get("contacts").cloned().unwrap_or_else(|| json!([]))}))
}

async fn send_message(
    runtime: &Runtime,
    request: &McpWorkerCallRequest,
    args: &Value,
) -> Result<Value, (&'static str, &'static str)> {
    queue(
        runtime,
        request,
        args,
        "send_message",
        SendContent {
            recipients: recipients(args)?,
            body: text(args)?,
            attachment_ids: Vec::new(),
        },
    )
    .await
}
async fn send_image(
    runtime: &Runtime,
    request: &McpWorkerCallRequest,
    args: &Value,
) -> Result<Value, (&'static str, &'static str)> {
    let key = args
        .get("idempotency_key")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or(("invalid_arguments", "idempotency_key is required."))?;
    let request_hash = canonical_request_hash("send_image", args)?;
    if let Some(queued) = runtime
        .client()
        .lookup_mcp_command(request.grant_id, "send_image", key, &request_hash)
        .map_err(|error| {
            if error == peppy_client_core::Error::Conflict {
                (
                    "idempotency_conflict",
                    "The idempotency key was reused with different input.",
                )
            } else {
                core_error(error)
            }
        })?
    {
        return Ok(
            json!({"message_id":queued.message_id,"command_id":queued.command_id,"conversation_id":queued.conversation_id,"attachment_ids":queued.attachment_ids,"status":"queued"}),
        );
    }
    let recipients = recipients(args)?;
    let conversation_id = conversation(args)?;
    let body = optional_text(args)?;
    validate_explicit_route(args)?;
    let image = args
        .get("image")
        .and_then(Value::as_str)
        .ok_or(("invalid_arguments", "image is required."))?;
    let bytes = decode_inline_image(image).await?;
    let path = runtime
        .state_dir()
        .join(format!("mcp-image-{}", Uuid::new_v4()));
    write_private_scratch(&path, &bytes)
        .map_err(|_| ("storage_unavailable", "Cannot prepare the image."))?;
    let prepared =
        runtime
            .client()
            .prepare_attachment(&path, raster_mime(&bytes).unwrap(), "mcp-image");
    let cleanup = fs::remove_file(&path);
    let prepared = match prepared {
        Ok(prepared) if cleanup.is_ok() => prepared,
        Ok(prepared) => {
            discard_prepared(runtime.client(), prepared.attachment_id)?;
            return Err(("storage_unavailable", "Cannot clean up the prepared image."));
        }
        Err(_) if cleanup.is_err() => {
            return Err(("storage_unavailable", "Cannot clean up the prepared image."));
        }
        Err(error) => return Err(core_error(error)),
    };
    let result = queue_with_conversation(
        runtime,
        request,
        args,
        "send_image",
        conversation_id.unwrap_or_else(ConversationId::new),
        SendContent {
            recipients,
            body,
            attachment_ids: vec![prepared.attachment_id],
        },
    )
    .await;
    let queued = matches!(
        &result,
        Ok(value) if value["attachment_ids"]
            .as_array()
            .is_some_and(|ids| {
                let attachment_id = prepared.attachment_id.to_string();
                ids.iter()
                    .any(|id| id.as_str() == Some(attachment_id.as_str()))
            })
    );
    if !queued {
        discard_prepared(runtime.client(), prepared.attachment_id)?;
    }
    result
}
async fn send_reaction_text(
    runtime: &Runtime,
    request: &McpWorkerCallRequest,
    args: &Value,
) -> Result<Value, (&'static str, &'static str)> {
    let target = id(args, "target_message_id")
        .map(MessageId)
        .map_err(|_| ("invalid_arguments", "target_message_id must be a UUID."))?;
    let message = runtime
        .client()
        .message_by_id(target)
        .map_err(core_error)?
        .ok_or(("not_found", "The target message is unavailable."))?;
    let conversation_id =
        conversation(args)?.ok_or(("invalid_arguments", "conversation_id is required."))?;
    if message.payload.record.conversation_id != conversation_id {
        return Err(("not_found", "The target message is unavailable."));
    }
    queue_with_conversation(
        runtime,
        request,
        args,
        "send_reaction_text",
        conversation_id,
        SendContent {
            recipients: runtime
                .client()
                .reply_recipients(target)
                .map_err(core_error)?,
            body: text(args)?,
            attachment_ids: Vec::new(),
        },
    )
    .await
}
struct SendContent {
    recipients: Vec<String>,
    body: String,
    attachment_ids: Vec<AttachmentId>,
}

async fn queue(
    runtime: &Runtime,
    request: &McpWorkerCallRequest,
    args: &Value,
    tool: &str,
    content: SendContent,
) -> Result<Value, (&'static str, &'static str)> {
    queue_with_conversation(
        runtime,
        request,
        args,
        tool,
        conversation(args)?.unwrap_or_else(ConversationId::new),
        content,
    )
    .await
}
async fn queue_with_conversation(
    runtime: &Runtime,
    request: &McpWorkerCallRequest,
    args: &Value,
    tool: &str,
    conversation_id: ConversationId,
    content: SendContent,
) -> Result<Value, (&'static str, &'static str)> {
    let key = args
        .get("idempotency_key")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or(("invalid_arguments", "idempotency_key is required."))?;
    let request_hash = canonical_request_hash(tool, args)?;
    if let Some(queued) = runtime
        .client()
        .lookup_mcp_command(request.grant_id, tool, key, &request_hash)
        .map_err(|error| {
            if error == peppy_client_core::Error::Conflict {
                (
                    "idempotency_conflict",
                    "The idempotency key was reused with different input.",
                )
            } else {
                core_error(error)
            }
        })?
    {
        return Ok(
            json!({"message_id":queued.message_id,"command_id":queued.command_id,"conversation_id":queued.conversation_id,"attachment_ids":queued.attachment_ids,"status":"queued"}),
        );
    }
    validate_explicit_route(args)?;
    let expected_transport = runtime
        .client()
        .mcp_send_transport(
            conversation_id,
            &content.recipients,
            &content.attachment_ids,
        )
        .map_err(core_error)?;
    let route = match select_route(
        runtime,
        args,
        expected_transport == peppy_client_core::Transport::Mms,
    )
    .await
    {
        Ok(route) => route,
        Err(result) => return Ok(result),
    };
    let queued = runtime
        .client()
        .queue_mcp_command(McpCommand {
            grant_id: request.grant_id,
            tool: tool.into(),
            idempotency_key: key.into(),
            conversation_id,
            recipients: content.recipients,
            body: content.body,
            attachment_ids: content.attachment_ids,
            route,
            expected_transport,
            request_hash,
        })
        .map_err(|error| {
            if error == peppy_client_core::Error::Conflict {
                (
                    "idempotency_conflict",
                    "The idempotency key was reused with different input.",
                )
            } else {
                core_error(error)
            }
        })?;
    runtime.request_sync();
    Ok(
        json!({"message_id":queued.message_id,"command_id":queued.command_id,"conversation_id":queued.conversation_id,"attachment_ids":queued.attachment_ids,"status":"queued"}),
    )
}
async fn select_route(runtime: &Runtime, args: &Value, mms: bool) -> Result<GatewayRoute, Value> {
    let views = runtime.gateway_views().await.map_err(|_| {
        error_result(
            "no_gateway",
            "No current SMS/MMS gateway route is available.",
        )
    })?;
    select_route_from_views(&views, args, mms)
}

fn select_route_from_views(
    views: &[GatewayView],
    args: &Value,
    mms: bool,
) -> Result<GatewayRoute, Value> {
    let eligible: Vec<&GatewayView> = views
        .iter()
        .filter(|view| {
            peppy_client_core::DeviceId::from_str(&view.id).is_ok()
                && !view.sim_id.is_empty()
                && view.sim_id.len() <= 256
                && view.supports_sms
                && (!mms || view.supports_mms)
                && !matches!(
                    view.availability,
                    GatewayAvailability::Offline
                        | GatewayAvailability::NeverConnected
                        | GatewayAvailability::NoSmsSim
                        | GatewayAvailability::SmsUnavailable
                        | GatewayAvailability::UnsupportedPlatform
                )
        })
        .collect();
    let view = match (
        args.get("gateway_id").and_then(Value::as_str),
        args.get("sim_id").and_then(Value::as_str),
    ) {
        (Some(id), Some(sim)) => eligible
            .into_iter()
            .find(|view| view.id == id && view.sim_id == sim)
            .ok_or_else(|| {
                error_result("no_gateway", "The selected gateway route is not eligible.")
            })?,
        (None, None) if eligible.len() == 1 => eligible[0],
        (None, None) if eligible.is_empty() => {
            return Err(error_result(
                "no_gateway",
                "No current SMS/MMS gateway route is available.",
            ));
        }
        _ => {
            let routes: Vec<_> = eligible.iter().take(MAX_ROUTE_CHOICES).map(|view| {
                let mut supported_types = vec!["sms"];
                if view.supports_mms { supported_types.push("mms"); }
                json!({"gateway_id":view.id,"sim_id":view.sim_id,"supported_types":supported_types})
            }).collect();
            let detail = json!({"code":"gateway_selection_required","message":"Select gateway_id and sim_id from routes.","routes":routes,"has_more":eligible.len() > MAX_ROUTE_CHOICES});
            return Err(
                json!({"content":[{"type":"text","text":detail.to_string()}],"isError":true}),
            );
        }
    };
    Ok(GatewayRoute {
        gateway_device_id: peppy_client_core::DeviceId::from_str(&view.id).map_err(|_| {
            error_result(
                "no_gateway",
                "No current SMS/MMS gateway route is available.",
            )
        })?,
        subscription_id: view.sim_id.clone(),
    })
}
fn conversation(args: &Value) -> Result<Option<ConversationId>, (&'static str, &'static str)> {
    args.get("conversation_id")
        .map(|value| {
            value
                .as_str()
                .ok_or(("invalid_arguments", "conversation_id must be a UUID."))
                .and_then(|value| {
                    Uuid::parse_str(value)
                        .map(ConversationId)
                        .map_err(|_| ("invalid_arguments", "conversation_id must be a UUID."))
                })
        })
        .transpose()
}

fn validate_explicit_route(args: &Value) -> Result<(), (&'static str, &'static str)> {
    match (args.get("gateway_id"), args.get("sim_id")) {
        (None, None) => Ok(()),
        (Some(Value::String(gateway_id)), Some(Value::String(sim_id)))
            if Uuid::parse_str(gateway_id).is_ok() && !sim_id.is_empty() && sim_id.len() <= 256 =>
        {
            Ok(())
        }
        _ => Err((
            "invalid_arguments",
            "gateway_id and sim_id must be a valid explicit route.",
        )),
    }
}

fn optional_text(args: &Value) -> Result<String, (&'static str, &'static str)> {
    match args.get("text") {
        None => Ok(String::new()),
        Some(Value::String(value)) if value.len() <= 64 * 1024 => Ok(value.clone()),
        _ => Err((
            "invalid_arguments",
            "text must be at most 65536 characters.",
        )),
    }
}

fn write_private_scratch(path: &std::path::Path, bytes: &[u8]) -> std::io::Result<()> {
    #[cfg(unix)]
    use std::os::unix::fs::OpenOptionsExt;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(path)?;
    use std::io::Write;
    let result = file.write_all(bytes).and_then(|()| file.sync_all());
    if let Err(error) = result {
        return match fs::remove_file(path) {
            Ok(()) => Err(error),
            Err(cleanup) if cleanup.kind() == std::io::ErrorKind::NotFound => Err(error),
            Err(_) => Err(std::io::Error::other("cannot clean up prepared image")),
        };
    }
    Ok(())
}

fn discard_prepared(
    client: &Client,
    attachment_id: AttachmentId,
) -> Result<(), (&'static str, &'static str)> {
    match client.discard_unreferenced_attachment(attachment_id) {
        Ok(true) => Ok(()),
        Ok(false) | Err(_) => Err((
            "storage_unavailable",
            "Cannot discard the unqueued prepared image.",
        )),
    }
}

fn canonical_request_hash(
    tool: &str,
    args: &Value,
) -> Result<Vec<u8>, (&'static str, &'static str)> {
    serde_json::to_vec(&(tool, args))
        .map(|request| Sha256::digest(request).to_vec())
        .map_err(|_| ("invalid_arguments", "arguments are invalid."))
}
fn recipients(args: &Value) -> Result<Vec<String>, (&'static str, &'static str)> {
    let values = args
        .get("recipients")
        .and_then(Value::as_array)
        .filter(|items| !items.is_empty() && items.len() <= 20)
        .ok_or((
            "invalid_arguments",
            "recipients must contain 1 to 20 phone numbers.",
        ))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|item| !item.is_empty() && item.len() <= 256)
                .map(str::to_owned)
                .ok_or((
                    "invalid_arguments",
                    "recipients must contain phone numbers.",
                ))
        })
        .collect()
}
fn text(args: &Value) -> Result<String, (&'static str, &'static str)> {
    args.get("text")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= 64 * 1024)
        .map(str::to_owned)
        .ok_or(("invalid_arguments", "text is required."))
}
async fn decode_inline_image(value: &str) -> Result<Vec<u8>, (&'static str, &'static str)> {
    let encoded = value
        .split_once(";base64,")
        .map(|(_, value)| value)
        .ok_or(("invalid_arguments", "image must be inline base64."))?;
    if encoded.len() > (MAX_ATTACHMENT_BYTES as usize).div_ceil(3) * 4 {
        return Err(("invalid_arguments", "image exceeds the inline limit."));
    }
    let bytes = STANDARD
        .decode(encoded)
        .map_err(|_| ("invalid_arguments", "image is not valid base64."))?;
    validate_raster_image(&bytes).await?;
    Ok(bytes)
}

async fn validate_raster_image(bytes: &[u8]) -> Result<&'static str, (&'static str, &'static str)> {
    if bytes.len() as u64 > MAX_ATTACHMENT_BYTES || raster_mime(bytes).is_none() {
        return Err((
            "invalid_arguments",
            "image must be a bounded PNG, JPEG, or WebP.",
        ));
    }
    let mime = raster_mime(bytes).expect("checked above");
    let bytes = bytes.to_vec();
    let permit = IMAGE_DECODE_SLOTS
        .clone()
        .acquire_owned()
        .await
        .map_err(|_| ("storage_unavailable", "The image decoder is unavailable."))?;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut reader = ImageReader::new(Cursor::new(bytes))
            .with_guessed_format()
            .map_err(|_| ("invalid_arguments", "image is malformed."))?;
        let mut limits = Limits::default();
        limits.max_image_width = Some(MAX_IMAGE_DIMENSION);
        limits.max_image_height = Some(MAX_IMAGE_DIMENSION);
        limits.max_alloc = Some(MAX_IMAGE_DECODE_ALLOCATION);
        reader.limits(limits);
        let image = reader.decode().map_err(|_| {
            (
                "invalid_arguments",
                "image dimensions or allocation exceed the limit.",
            )
        })?;
        debug_assert!(
            image.width() <= MAX_IMAGE_DIMENSION && image.height() <= MAX_IMAGE_DIMENSION
        );
        Ok(mime)
    })
    .await
    .map_err(|_| ("storage_unavailable", "The image decoder is unavailable."))?
}

fn tool(name: &str, description: &str, input_schema: Value) -> Value {
    json!({"name":name,"description":description,"inputSchema":input_schema})
}
fn write_schema(reaction: bool) -> Value {
    let mut schema = if reaction {
        json!({"type":"object","required":["conversation_id","target_message_id","text","idempotency_key"],"properties":{"conversation_id":{"type":"string","format":"uuid"},"target_message_id":{"type":"string","format":"uuid"},"text":{"type":"string","minLength":1,"maxLength":65536},"idempotency_key":{"type":"string","minLength":1,"maxLength":256}},"additionalProperties":false})
    } else {
        json!({"type":"object","required":["recipients","text","idempotency_key"],"properties":{"conversation_id":{"type":"string","format":"uuid"},"recipients":{"type":"array","minItems":1,"maxItems":20,"items":{"type":"string"}},"text":{"type":"string","minLength":1,"maxLength":65536},"idempotency_key":{"type":"string","minLength":1,"maxLength":256}},"additionalProperties":false})
    };
    schema["properties"]["gateway_id"] = json!({"type":"string","format":"uuid"});
    schema["properties"]["sim_id"] = json!({"type":"string","minLength":1,"maxLength":256});
    schema
}
fn image_schema() -> Value {
    json!({"type":"object","required":["recipients","image","idempotency_key"],"properties":{"conversation_id":{"type":"string","format":"uuid"},"gateway_id":{"type":"string","format":"uuid"},"sim_id":{"type":"string"},"recipients":{"type":"array","minItems":1,"maxItems":20,"items":{"type":"string"}},"text":{"type":"string","maxLength":65536},"image":{"type":"string","description":"inline PNG, JPEG, or WebP data URL"},"idempotency_key":{"type":"string","minLength":1,"maxLength":256}},"additionalProperties":false})
}
fn id(args: &Value, field: &str) -> Result<Uuid, ()> {
    args.get(field)
        .and_then(Value::as_str)
        .ok_or(())
        .and_then(|value| Uuid::parse_str(value).map_err(|_| ()))
}
fn bounded_limit(args: &Value, maximum: usize) -> Result<usize, (&'static str, &'static str)> {
    let value = match args.get("limit") {
        None => maximum as u64,
        Some(value) => value
            .as_u64()
            .ok_or(("invalid_arguments", "limit must be an integer."))?,
    };
    usize::try_from(value)
        .ok()
        .filter(|value| *value > 0 && *value <= maximum)
        .ok_or(("invalid_arguments", "limit is outside the allowed range."))
}
fn raster_mime(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) {
        Some("image/jpeg")
    } else if bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
        Some("image/webp")
    } else {
        None
    }
}
fn core_error(error: peppy_client_core::Error) -> (&'static str, &'static str) {
    match error {
        peppy_client_core::Error::InvalidRequest(_) => {
            ("invalid_arguments", "The supplied arguments are invalid.")
        }
        peppy_client_core::Error::Conflict => (
            "idempotency_conflict",
            "The idempotency key was reused with different input.",
        ),
        peppy_client_core::Error::NotFound => {
            ("not_found", "The requested local item is unavailable.")
        }
        _ => ("core_error", "Local data is unavailable."),
    }
}
fn success(value: Value) -> Value {
    json!({"content":[{"type":"text","text":value.to_string()}],"isError":false})
}

fn serialized_success_len(value: &Value) -> usize {
    serde_json::to_vec(&success(value.clone()))
        .map(|value| value.len())
        .unwrap_or(usize::MAX)
}

fn escaped_json_string_content_len(value: &Value) -> usize {
    serde_json::to_string(&value.to_string())
        .map(|value| value.len().saturating_sub(2))
        .unwrap_or(usize::MAX)
}
fn error_result(code: &str, message: &str) -> Value {
    json!({"content":[{"type":"text","text":json!({"code":code,"message":message}).to_string()}],"isError":true})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalogue_filters_write_tools_and_never_advertises_partial_grants() {
        let read = list_tools(&[McpScope::ConversationsRead]);
        let names: Vec<_> = read["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"list_conversations"));
        assert!(!names.contains(&"send_message"));
        let image_only = list_tools(&[McpScope::MediaSend]);
        assert!(
            image_only["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "send_image")
        );
        assert!(
            !image_only["tools"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tool| tool["name"] == "send_message")
        );
    }

    #[tokio::test]
    async fn inline_image_rejects_svg_and_oversize_content() {
        assert!(
            decode_inline_image("data:image/svg+xml;base64,PHN2Zy8+")
                .await
                .is_err()
        );
        assert!(raster_mime(b"<svg").is_none());
    }

    #[tokio::test]
    async fn decoder_rejects_truncated_raster_bytes() {
        assert!(
            decode_inline_image("data:image/png;base64,iVBORw0KGgo=")
                .await
                .is_err()
        );
    }

    #[test]
    fn image_schema_accepts_route_selection_and_requires_image() {
        let schema = image_schema();
        assert!(
            schema["required"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field == "image")
        );
        assert!(schema["properties"].get("gateway_id").is_some());
        assert!(schema["properties"].get("sim_id").is_some());
    }

    #[test]
    fn all_send_schemas_accept_explicit_route_fields() {
        for schema in [write_schema(false), write_schema(true), image_schema()] {
            assert_eq!(schema["additionalProperties"], false);
            assert_eq!(schema["properties"]["gateway_id"]["format"], "uuid");
            assert_eq!(schema["properties"]["sim_id"]["type"], "string");
        }
    }

    #[test]
    fn malformed_explicit_routes_and_limits_are_rejected_before_queueing() {
        assert!(bounded_limit(&json!({"limit":"100"}), MAX_MESSAGES).is_err());
        assert!(bounded_limit(&json!({"limit":0}), MAX_MESSAGES).is_err());
        assert!(validate_explicit_route(&json!({"gateway_id":Uuid::new_v4()})).is_err());
        assert!(validate_explicit_route(&json!({"gateway_id":4,"sim_id":"sim-1"})).is_err());
        assert!(
            validate_explicit_route(&json!({"gateway_id":Uuid::new_v4(),"sim_id":""})).is_err()
        );
    }

    #[tokio::test]
    async fn dispatch_rejects_malformed_routes_without_queueing_text_or_reactions() {
        use peppy_client_core::IncomingSms;

        let directory = tempfile::tempdir().unwrap();
        let runtime = Runtime::test_runtime_for_registry(
            directory.path().to_owned(),
            "http://127.0.0.1:1".into(),
        );
        let request = |name, arguments| McpWorkerCallRequest {
            vault_id: runtime.vault_id().0,
            device_id: runtime.device_id().0,
            grant_id: Uuid::new_v4(),
            scopes: vec![McpScope::MessagesSend],
            request: json!({"params":{"name":name,"arguments":arguments}}),
        };
        let text = call_tool(
            &runtime,
            &request(
                "send_message",
                json!({"recipients":["15551234567"],"text":"hello","idempotency_key":"text","gateway_id":null,"sim_id":false}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(text["isError"], true);
        assert_eq!(tool_error_code(&text), "invalid_arguments");
        assert_eq!(runtime.client().pending_outbox_count().unwrap(), 0);

        let incoming = runtime
            .client()
            .capture_incoming(IncomingSms {
                conversation_id: None,
                sender_address: "15551234567".into(),
                body: "incoming".into(),
                provider_message_id: Some("reaction-target".into()),
                imported: false,
            })
            .unwrap();
        let outbox_before_reaction = runtime.client().pending_outbox_count().unwrap();
        let reaction = call_tool(
            &runtime,
            &request(
                "send_reaction_text",
                json!({"conversation_id":incoming.conversation_id,"target_message_id":incoming.message_id,"text":"ok","idempotency_key":"reaction","gateway_id":Uuid::new_v4()}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(reaction["isError"], true);
        assert_eq!(tool_error_code(&reaction), "invalid_arguments");
        assert_eq!(
            runtime.client().pending_outbox_count().unwrap(),
            outbox_before_reaction
        );
    }

    #[tokio::test]
    async fn no_route_image_dispatch_discards_the_prepared_attachment() {
        let directory = tempfile::tempdir().unwrap();
        let runtime = Runtime::test_runtime_for_registry(
            directory.path().to_owned(),
            "http://127.0.0.1:1".into(),
        );
        let image = png(1, 1);
        let request = McpWorkerCallRequest {
            vault_id: runtime.vault_id().0,
            device_id: runtime.device_id().0,
            grant_id: Uuid::new_v4(),
            scopes: vec![McpScope::MediaSend],
            request: json!({"params":{"name":"send_image","arguments":{"recipients":["15551234567"],"image":format!("data:image/png;base64,{}", STANDARD.encode(image)),"idempotency_key":"no-route"}}}),
        };
        let before = file_inventory(directory.path());
        let result = call_tool(&runtime, &request).await.unwrap();
        assert_eq!(result["isError"], true);
        assert_eq!(tool_error_code(&result), "no_gateway");
        assert_eq!(file_inventory(directory.path()), before);
        let repeated = call_tool(&runtime, &request).await.unwrap();
        assert_eq!(tool_error_code(&repeated), "no_gateway");
        assert_eq!(file_inventory(directory.path()), before);
        assert_eq!(runtime.client().pending_outbox_count().unwrap(), 0);
    }

    fn tool_error_code(result: &Value) -> String {
        let detail: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        detail["code"].as_str().unwrap().to_owned()
    }

    fn file_inventory(path: &std::path::Path) -> Vec<std::path::PathBuf> {
        fn visit(
            root: &std::path::Path,
            path: &std::path::Path,
            files: &mut Vec<std::path::PathBuf>,
        ) {
            for entry in fs::read_dir(path).unwrap() {
                let entry = entry.unwrap();
                let path = entry.path();
                if path.is_dir() {
                    visit(root, &path, files);
                } else {
                    files.push(path.strip_prefix(root).unwrap().to_owned());
                }
            }
        }
        let mut files = Vec::new();
        visit(path, path, &mut files);
        files.sort();
        files
    }

    fn gateway(id: String, sim: &str, mms: bool) -> GatewayView {
        serde_json::from_value(json!({"id":id,"name":"private phone name","simId":sim,
            "online":true,"simulated":false,"supportsSms":true,"supportsMms":mms,
            "availability":"connected"}))
        .unwrap()
    }

    #[test]
    fn route_selection_error_exposes_only_bounded_validated_runtime_choices() {
        let mut views: Vec<_> = (0..MAX_ROUTE_CHOICES + 1)
            .map(|_| gateway(Uuid::new_v4().to_string(), "sim-1", true))
            .collect();
        views.push(gateway("invalid uuid".into(), "sim-1", true));
        views.push(gateway(Uuid::new_v4().to_string(), "", true));
        let result = select_route_from_views(&views, &json!({}), true).unwrap_err();
        assert_eq!(result["isError"], true);
        let detail: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(detail["code"], "gateway_selection_required");
        assert_eq!(detail["has_more"], true);
        let routes = detail["routes"].as_array().unwrap();
        assert_eq!(routes.len(), MAX_ROUTE_CHOICES);
        for route in routes {
            assert_eq!(route.as_object().unwrap().len(), 3);
            assert_eq!(route["supported_types"], json!(["sms", "mms"]));
            assert!(select_route_from_views(&views, route, true).is_ok());
        }
        assert!(!result.to_string().contains("private phone name"));
        let result = select_route_from_views(
            &[gateway(Uuid::new_v4().to_string(), "sms-only", false)],
            &json!({}),
            true,
        )
        .unwrap_err();
        let detail: Value =
            serde_json::from_str(result["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(detail["code"], "no_gateway");
    }

    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut bytes = Cursor::new(Vec::new());
        image::DynamicImage::new_luma8(width, height)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        bytes.into_inner()
    }

    #[tokio::test]
    async fn attachment_output_and_inline_input_share_decoder_and_resource_limits() {
        let valid = png(1, 1);
        assert!(attachment_result(&valid).await.is_ok());
        for bytes in [
            b"\x89PNG\r\n\x1a\n".to_vec(),
            png(4097, 1),
            vec![0; MAX_ATTACHMENT_BYTES as usize + 1],
        ] {
            assert!(attachment_result(&bytes).await.is_err());
            assert!(
                decode_inline_image(&format!(
                    "data:image/png;base64,{}",
                    STANDARD.encode(&bytes)
                ))
                .await
                .is_err()
            );
        }
        assert!(attachment_result(&png(1024, 1024)).await.is_ok());
        assert!(attachment_result(&png(1280, 720)).await.is_ok());
    }

    #[test]
    fn core_errors_preserve_argument_and_idempotency_failures() {
        assert_eq!(
            core_error(peppy_client_core::Error::InvalidRequest("request")),
            ("invalid_arguments", "The supplied arguments are invalid.")
        );
        assert_eq!(
            core_error(peppy_client_core::Error::Conflict),
            (
                "idempotency_conflict",
                "The idempotency key was reused with different input."
            )
        );
        assert_eq!(
            core_error(peppy_client_core::Error::NotFound),
            ("not_found", "The requested local item is unavailable.")
        );
    }

    #[test]
    fn canonical_hash_and_replay_ignore_transport_reclassification() {
        use peppy_client_core::{
            ClientConfig, DatabaseKey, DeviceId, IncomingMms, Transport, VaultId,
        };
        use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};

        let dir = tempfile::tempdir().unwrap();
        let vault_id = VaultId::new();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("tools.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let phrase = "mcp commands test phrase";
        let root = derive_root_key(phrase, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        client.unlock(&profile, &header, phrase).unwrap();

        let conversation_id = ConversationId::new();
        let args = json!({
            "conversation_id": conversation_id.to_string(),
            "recipients": ["15551234567"],
            "text": "reply",
            "idempotency_key": "key",
        });
        let request_hash = canonical_request_hash("send_message", &args).unwrap();
        let command = McpCommand {
            grant_id: Uuid::new_v4(),
            tool: "send_message".into(),
            idempotency_key: "key".into(),
            conversation_id,
            recipients: vec!["15551234567".into()],
            body: "reply".into(),
            attachment_ids: Vec::new(),
            route: GatewayRoute {
                gateway_device_id: DeviceId::new(),
                subscription_id: "sim-1".into(),
            },
            expected_transport: Transport::Sms,
            request_hash: request_hash.clone(),
        };
        let first = client.queue_mcp_command(command.clone()).unwrap();
        client
            .capture_incoming_mms(IncomingMms {
                conversation_id: Some(conversation_id),
                sender_address: "15551234567".into(),
                body: "picture message".into(),
                provider_message_id: Some("mms-1".into()),
                imported: false,
                attachment_ids: Vec::new(),
                recipients: vec!["15551234567".into()],
                subject: None,
            })
            .unwrap();

        assert_eq!(
            client
                .mcp_send_transport(conversation_id, &command.recipients, &[])
                .unwrap(),
            Transport::Mms
        );
        assert_eq!(
            canonical_request_hash("send_message", &args).unwrap(),
            request_hash
        );
        assert_eq!(client.queue_mcp_command(command).unwrap(), first);
    }

    #[test]
    fn message_pages_stay_within_the_outer_escaped_tool_envelope_and_continue() {
        use peppy_client_core::{ClientConfig, DatabaseKey, DeviceId, Transport, VaultId};
        use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};

        let dir = tempfile::tempdir().unwrap();
        let vault_id = VaultId::new();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("tools.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let root = derive_root_key("mcp commands test phrase", &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        client
            .unlock(&profile, &header, "mcp commands test phrase")
            .unwrap();

        let conversation_id = ConversationId::new();
        for index in 0..100 {
            client
                .queue_mcp_command(McpCommand {
                    grant_id: Uuid::new_v4(),
                    tool: "send_message".into(),
                    idempotency_key: format!("page-{index}"),
                    conversation_id,
                    recipients: vec!["15551234567".into()],
                    body: "\"\\\n".repeat(16 * 1024),
                    attachment_ids: Vec::new(),
                    route: GatewayRoute {
                        gateway_device_id: DeviceId::new(),
                        subscription_id: "sim-1".into(),
                    },
                    expected_transport: Transport::Sms,
                    request_hash: vec![index as u8; 32],
                })
                .unwrap();
        }

        let expected: Vec<_> = client
            .messages_page(conversation_id, None, MAX_MESSAGES)
            .unwrap()
            .messages
            .into_iter()
            .map(|message| json!(message.payload.record.message_id))
            .collect();
        let mut before: Option<Value> = None;
        let mut pages = Vec::new();
        loop {
            let mut args = json!({"conversation_id":conversation_id,"limit":MAX_MESSAGES});
            if let Some(before) = &before {
                args["before"] = before.clone();
            }
            let page = get_messages(&client, &args).unwrap();
            assert!(serialized_success_len(&page) <= MAX_TOOL_RESULT_BYTES);
            let ids: Vec<_> = page["messages"]
                .as_array()
                .unwrap()
                .iter()
                .map(|message| message["message_id"].clone())
                .collect();
            if !page["has_more"].as_bool().unwrap() {
                pages.push(ids);
                break;
            }
            before = ids.first().cloned();
            pages.push(ids);
        }
        pages.reverse();
        assert_eq!(pages.into_iter().flatten().collect::<Vec<_>>(), expected);
    }

    #[tokio::test]
    async fn message_attachment_references_are_discoverable_and_fetchable() {
        use peppy_client_core::{ClientConfig, DatabaseKey, DeviceId, VaultId};
        use peppy_crypto::{KeyProfile, create_vault_check_header, derive_root_key};
        let dir = tempfile::tempdir().unwrap();
        let vault_id = VaultId::new();
        let client = Client::open(
            ClientConfig {
                database_path: dir.path().join("tools.db"),
                vault_id,
                device_id: DeviceId::new(),
            },
            DatabaseKey::new(&[9; 32]).unwrap(),
        )
        .unwrap();
        let profile = KeyProfile::new(vault_id.0, 1).unwrap();
        let phrase = "mcp commands test phrase";
        let root = derive_root_key(phrase, &profile).unwrap();
        let header = create_vault_check_header(&root, profile.clone()).unwrap();
        client.unlock(&profile, &header, phrase).unwrap();
        let path = dir.path().join("image.png");
        fs::write(&path, png(1, 1)).unwrap();
        let attachment = client
            .prepare_attachment(&path, "image/png", "image.png")
            .unwrap();
        let conversation_id = ConversationId::new();
        client
            .queue_mcp_command(McpCommand {
                grant_id: Uuid::new_v4(),
                tool: "send_image".into(),
                idempotency_key: "image".into(),
                conversation_id,
                recipients: vec!["15551234567".into()],
                body: "image".into(),
                attachment_ids: vec![attachment.attachment_id],
                route: GatewayRoute {
                    gateway_device_id: DeviceId::new(),
                    subscription_id: "sim-1".into(),
                },
                expected_transport: peppy_client_core::Transport::Mms,
                request_hash: vec![1; 32],
            })
            .unwrap();
        let messages = get_messages(&client, &json!({"conversation_id":conversation_id})).unwrap();
        let descriptor = &messages["messages"][0]["attachments"][0];
        assert_eq!(descriptor["attachment_id"], json!(attachment.attachment_id));
        assert_eq!(descriptor["mime"], "image/png");
        assert_eq!(descriptor["name"], "image.png");
        assert_eq!(descriptor["size"], attachment.plaintext_bytes);
        assert!(
            get_attachment(
                &client,
                &json!({"attachment_id":descriptor["attachment_id"]})
            )
            .await
            .is_ok()
        );
        assert_eq!(
            attachment_descriptors(&client, std::iter::repeat(attachment.attachment_id)).len(),
            MAX_ATTACHMENT_DESCRIPTORS
        );
        fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        let truncated = client
            .prepare_attachment(&path, "image/png", "bad.png")
            .unwrap();
        assert!(
            get_attachment(&client, &json!({"attachment_id":truncated.attachment_id}))
                .await
                .is_err()
        );
    }
}
