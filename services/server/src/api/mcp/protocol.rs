use std::sync::Arc;

use axum::{
    Router,
    extract::{Request, State},
    http::{HeaderValue, StatusCode, header::CACHE_CONTROL},
    response::{IntoResponse, Response},
    routing::any,
};
use http_body_util::{BodyExt, combinators::BoxBody};
use peppy_hosted_api::mcp::{McpScope, McpWorkerCallRequest};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, ListToolsResult,
        PaginatedRequestParams, ServerCapabilities, ServerConfig,
    },
    service::RequestContext,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::never::NeverSessionManager,
    },
};

use super::{
    super::{ApiState, Operation},
    oauth::{McpPrincipal, authenticate_mcp, revalidate_mcp},
    worker_request,
};

const MAX_PUBLIC_BODY_BYTES: usize = 4 * 1024 * 1024;

pub(super) fn routes() -> Router<ApiState> {
    Router::new().route("/mcp", any(handle))
}

async fn handle(State(state): State<ApiState>, mut request: Request) -> Response {
    let principal = match authenticate_mcp(&state, request.headers()).await {
        Ok(principal) => principal,
        Err(response) => return response,
    };
    request
        .extensions_mut()
        .insert(McpRequestPrincipal::from(&principal));

    let allowed_hosts = state
        .instance_origin
        .as_ref()
        .map(|origin| origin.authority())
        .map(|authority| vec![authority.to_owned()])
        .unwrap_or_default();
    let allowed_origins = state
        .instance_origin
        .as_ref()
        .map(|origin| vec![origin.origin().ascii_serialization()])
        .unwrap_or_default();
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true)
        .with_max_request_body_bytes(MAX_PUBLIC_BODY_BYTES)
        .with_allowed_hosts(allowed_hosts)
        .with_allowed_origins(allowed_origins)
        .enforce_origin_validation();
    let handler_state = state.clone();
    let handler_principal = Arc::new(principal);
    let service = StreamableHttpService::new(
        move || {
            Ok(McpServer {
                state: handler_state.clone(),
                principal: handler_principal.clone(),
            })
        },
        Arc::new(NeverSessionManager::default()),
        config,
    );
    let mut response = bounded_response(service.handle(request).await).await;
    response
        .headers_mut()
        .insert(CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.map(axum::body::Body::new)
}

async fn bounded_response(
    response: axum::http::Response<BoxBody<axum::body::Bytes, std::convert::Infallible>>,
) -> Response {
    let (parts, mut body) = response.into_parts();
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = match frame {
            Ok(frame) => frame,
            Err(never) => match never {},
        };
        if let Ok(data) = frame.into_data() {
            if bytes.len().saturating_add(data.len()) > MAX_PUBLIC_BODY_BYTES {
                return StatusCode::BAD_GATEWAY.into_response();
            }
            bytes.extend_from_slice(&data);
        }
    }
    axum::http::Response::from_parts(parts, axum::body::Body::from(bytes))
}

#[derive(Clone)]
struct McpServer {
    state: ApiState,
    principal: Arc<McpPrincipal>,
}

/// Immutable request-local identity exposed to SDK handlers through request extensions.
#[derive(Clone)]
struct McpRequestPrincipal {
    vault_id: uuid::Uuid,
    device_id: uuid::Uuid,
    grant_id: uuid::Uuid,
    scopes: Vec<McpScope>,
}

impl From<&McpPrincipal> for McpRequestPrincipal {
    fn from(principal: &McpPrincipal) -> Self {
        Self {
            vault_id: principal.vault_id,
            device_id: principal.device_id,
            grant_id: principal.grant_id,
            scopes: principal.scopes.clone(),
        }
    }
}

impl ServerHandler for McpServer {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
    }

    async fn list_tools(
        &self,
        request: Option<PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        self.validate_request_principal(&context)?;
        if self.authorize_tool("list_conversations").await? == ToolAccess::PolicyDenied {
            return Err(McpError::invalid_request("operation_not_permitted", None));
        }
        let result = self
            .worker_call(
                "tools/list",
                serde_json::to_value(request).map_err(invalid_worker_result)?,
            )
            .await?;
        let mut result =
            serde_json::from_value::<ListToolsResult>(result).map_err(invalid_worker_result)?;
        let mut permitted = Vec::with_capacity(result.tools.len());
        for tool in result.tools {
            if tool_allowed(tool.name.as_ref(), &self.principal.scopes)
                && matches!(
                    self.authorize_tool(tool.name.as_ref()).await,
                    Ok(ToolAccess::Permitted)
                )
            {
                permitted.push(tool);
            }
        }
        result.tools = permitted;
        Ok(result)
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        self.validate_request_principal(&context)?;
        if !tool_allowed(request.name.as_ref(), &self.principal.scopes) {
            return Err(McpError::invalid_params("tool is not available", None));
        }
        if self.authorize_tool(request.name.as_ref()).await? == ToolAccess::PolicyDenied {
            return Ok(CallToolResponse::Complete(CallToolResult::error(vec![
                ContentBlock::text(
                    "operation_not_permitted: this Peppy account does not currently allow this operation.",
                ),
            ])));
        }
        let result = self
            .worker_call(
                "tools/call",
                serde_json::to_value(request).map_err(invalid_worker_result)?,
            )
            .await?;
        serde_json::from_value(result)
            .map(CallToolResponse::Complete)
            .map_err(invalid_worker_result)
    }
}

impl McpServer {
    fn validate_request_principal(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(), McpError> {
        let principal = context
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|parts| parts.extensions.get::<McpRequestPrincipal>())
            .ok_or_else(|| McpError::internal_error("request identity is unavailable", None))?;
        if principal.vault_id != self.principal.vault_id
            || principal.device_id != self.principal.device_id
            || principal.grant_id != self.principal.grant_id
            || principal.scopes != self.principal.scopes
        {
            return Err(McpError::internal_error("request identity changed", None));
        }
        Ok(())
    }

    /// Unknown names are a protocol error; a policy denial is reported by the caller.
    async fn authorize_tool(&self, name: &str) -> Result<ToolAccess, McpError> {
        let operations: &[Operation] = match name {
            "list_conversations" | "get_messages" | "get_attachment" | "list_contacts" => {
                &[Operation::Read]
            }
            "send_message" | "send_reaction_text" => &[Operation::Send],
            "send_image" => &[Operation::Send, Operation::Upload],
            _ => return Err(McpError::invalid_params("tool is not available", None)),
        };
        for operation in operations {
            if !self
                .state
                .policy
                .authorize(self.principal.access_principal(), *operation)
                .await
            {
                return Ok(ToolAccess::PolicyDenied);
            }
        }
        Ok(ToolAccess::Permitted)
    }

    async fn worker_call(
        &self,
        method: &'static str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, McpError> {
        revalidate_mcp(&self.state, &self.principal)
            .await
            .map_err(|_| McpError::internal_error("authorization is no longer valid", None))?;
        let request = McpWorkerCallRequest {
            vault_id: self.principal.vault_id,
            device_id: self.principal.device_id,
            grant_id: self.principal.grant_id,
            scopes: self.principal.scopes.clone(),
            request: serde_json::json!({"method": method, "params": params}),
        };
        let result = worker_request(
            &self.state,
            "/internal/mcp/call",
            &serde_json::to_value(request).map_err(invalid_worker_result)?,
        )
        .await
        .map_err(|_| McpError::internal_error("MCP worker is unavailable", None))?;
        if serde_json::to_vec(&result).map_or(true, |value| value.len() > MAX_PUBLIC_BODY_BYTES) {
            return Err(McpError::internal_error(
                "MCP worker returned an invalid result",
                None,
            ));
        }
        revalidate_mcp(&self.state, &self.principal)
            .await
            .map_err(|_| McpError::internal_error("authorization is no longer valid", None))?;
        Ok(result)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ToolAccess {
    Permitted,
    PolicyDenied,
}

fn invalid_worker_result(_: impl std::fmt::Display) -> McpError {
    McpError::internal_error("MCP worker returned an invalid result", None)
}

fn tool_allowed(name: &str, scopes: &[McpScope]) -> bool {
    let required_scope = match name {
        "list_conversations" | "get_messages" | "get_attachment" | "list_contacts" => {
            McpScope::ConversationsRead
        }
        "send_message" | "send_reaction_text" => McpScope::MessagesSend,
        "send_image" => McpScope::MediaSend,
        _ => return false,
    };
    scopes.contains(&required_scope)
}

#[cfg(test)]
mod tests {
    use peppy_hosted_api::mcp::McpScope;

    use super::tool_allowed;

    #[test]
    fn tool_catalogue_is_scoped_to_the_grant() {
        assert!(tool_allowed(
            "list_conversations",
            &[McpScope::ConversationsRead]
        ));
        assert!(!tool_allowed(
            "send_message",
            &[McpScope::ConversationsRead]
        ));
        assert!(tool_allowed("send_image", &[McpScope::MediaSend]));
        assert!(!tool_allowed("unknown", &[McpScope::MediaSend]));
    }
}
