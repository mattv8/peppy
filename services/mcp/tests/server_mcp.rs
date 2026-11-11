#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_client_core::{
    Client as CoreClient, ClientConfig, DatabaseKey, DeviceId, IncomingMms, Transport, VaultId,
};
use peppy_crypto::{
    KeyProfile, create_vault_check_header, derive_root_key, enrollment::derive_admission_keypair,
};
use peppy_hosted_api::{
    computer_enrollment::{ComputerEnrollmentCreated, ComputerEnrollmentStatus},
    enrollment::AdmissionComplete,
    mcp::{McpDeviceStatus, McpEnrollmentPreparation, McpGrantView, McpWorkerActivateResponse},
};
use peppy_hosted_client::newenrollment::{ClientKind, EnrollmentClient};
use peppy_server::{
    ServerBuilder,
    config::{Config, S3Config},
};
use sha2::{Digest, Sha256};
use sqlx::postgres::PgPoolOptions;
use tempfile::TempDir;
use tokio::net::TcpListener;
use url::Url;
use uuid::Uuid;

const PASSPHRASE: &str = "correct horse battery staple";
const CANONICAL_ORIGIN: &str = "https://mcp-e2e.peppy.test";
const INSTANCE_SESSION: &str = "mcp-e2e-owner-session";

const HTTP_TIMEOUT: Duration = Duration::from_secs(30);
const SMOKE_TIMEOUT: Duration = Duration::from_secs(600);

#[tokio::test]
async fn owner_enrolls_the_actual_worker_through_server_management_routes() {
    tokio::time::timeout(SMOKE_TIMEOUT, run_owner_enrollment_smoke())
        .await
        .expect("MCP smoke exceeded its 600s budget");
}

async fn run_owner_enrollment_smoke() {
    let temp = TempDir::new().unwrap();
    let database_url = std::env::var("TEST_DATABASE_URL").expect("TEST_DATABASE_URL required");
    let admin = PgPoolOptions::new()
        .max_connections(1)
        .connect(&database_url)
        .await
        .unwrap();
    let schema = format!("peppy_mcp_e2e_{}", Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .unwrap();
    let _schema_guard = SchemaGuard {
        database_url: database_url.clone(),
        schema: schema.clone(),
    };
    let mut database: Url = database_url.parse().unwrap();
    database
        .query_pairs_mut()
        .append_pair("options", &format!("-csearch_path={schema}"));
    let pool = PgPoolOptions::new()
        .max_connections(8)
        .connect(database.as_str())
        .await
        .unwrap();
    peppy_server::MIGRATOR.run(&pool).await.unwrap();

    let server_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let server_origin = format!("http://{}", server_listener.local_addr().unwrap());
    let worker_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let worker_origin = format!("http://{}", worker_listener.local_addr().unwrap());
    let worker_addr = worker_listener.local_addr().unwrap();
    drop(worker_listener);
    let token_file = temp.path().join("control-token");
    std::fs::write(&token_file, "e2e-control-token-that-is-long-enough\n").unwrap();
    #[cfg(unix)]
    std::fs::set_permissions(&token_file, std::fs::Permissions::from_mode(0o600)).unwrap();
    let worker_state_dir = temp.path().join("worker");
    let worker_key_file = temp.path().join("worker-key");
    let mut worker = start_worker(
        worker_addr,
        &server_origin,
        &worker_state_dir,
        &worker_key_file,
        &token_file,
    )
    .await;

    let vault = Uuid::new_v4();
    let profile = KeyProfile::new(vault, 1).unwrap();
    let root = derive_root_key(PASSPHRASE, &profile).unwrap();
    let header = create_vault_check_header(&root, profile.clone()).unwrap();
    let owner = peppy_server::api::create_owner(
        &pool,
        serde_json::to_value(&profile).unwrap(),
        serde_json::to_vec(&header).unwrap(),
        profile.fingerprint().unwrap(),
        1,
    )
    .await
    .unwrap();
    let admission = derive_admission_keypair(&root).unwrap();
    sqlx::query("INSERT INTO vault_admission_keys(vault_id,key_epoch,profile_fingerprint,admission_public_key) VALUES($1,1,$2,$3)")
        .bind(vault)
        .bind(profile.fingerprint().unwrap())
        .bind(admission.public_key().as_slice())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO vault_owners(vault_id,username,password_hash,operation_id,request_digest) VALUES($1,'mcp-e2e','test',$2,$3)")
        .bind(vault)
        .bind(Uuid::new_v4())
        .bind(vec![0_u8; 32])
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO instance_sessions(token_digest,vault_id,expires_at) VALUES($1,$2,now()+interval '1 hour')")
        .bind(peppy_server::instance::digest(INSTANCE_SESSION.as_bytes()).as_slice())
        .bind(vault)
        .execute(&pool)
        .await
        .unwrap();
    let owner_client = CoreClient::open(
        ClientConfig {
            database_path: temp.path().join("owner.sqlite"),
            vault_id: VaultId(vault),
            device_id: DeviceId(owner.device_id),
        },
        DatabaseKey::new(&[7; 32]).unwrap(),
    )
    .unwrap();
    owner_client.unlock(&profile, &header, PASSPHRASE).unwrap();
    let config = Config {
        bind_addr: server_listener.local_addr().unwrap(),
        database_url: database.to_string(),
        release_identity: "e2e".into(),
        s3: Some(S3Config {
            endpoint: std::env::var("S3_INTERNAL_ENDPOINT")
                .expect("S3_INTERNAL_ENDPOINT required (source test-env.sh)")
                .parse()
                .expect("S3_INTERNAL_ENDPOINT must be a URL"),
            bucket: std::env::var("S3_BUCKET").expect("S3_BUCKET required (source test-env.sh)"),
            access_key: std::env::var("S3_ACCESS_KEY")
                .expect("S3_ACCESS_KEY required (source test-env.sh)"),
            secret_key: std::env::var("S3_SECRET_KEY")
                .expect("S3_SECRET_KEY required (source test-env.sh)"),
            signing_region: "us-east-1".into(),
            precreated_bucket: true,
            readiness_timeout: Duration::from_secs(5),
        }),
        public_api_url: Some(CANONICAL_ORIGIN.parse().unwrap()),
        public_attachment_url: None,
        vault_attachment_quota_bytes: 512 * 1024 * 1024,
        trusted_proxy_cidrs: vec![],
        replay_retention: Duration::from_secs(60),
        relay_url: None,
        mcp_worker_url: Some(worker_origin.parse().unwrap()),
        mcp_worker_token_file: Some(token_file.clone()),
        web_client: None,
        web_live_trusted_origin: None,
    };
    let maintenance_pool = PgPoolOptions::new()
        .max_connections(2)
        .connect(database.as_str())
        .await
        .unwrap();
    let mut server = ServerBuilder::new(config, pool.clone())
        .maintenance_database(maintenance_pool.clone())
        .build()
        .await
        .unwrap();
    server.start_maintenance().unwrap();
    let router = server.router();
    let server_task = tokio::spawn(async move {
        axum::serve(server_listener, router).await.unwrap();
    });

    let http = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap();
    let prepared: McpEnrollmentPreparation = post(
        &http,
        &server_origin,
        "/v1/mcp/device/prepare",
        &owner.device_token,
        &(),
    )
    .await;
    let mut enrollment = EnrollmentClient::for_existing_owner(
        CANONICAL_ORIGIN.into(),
        ClientKind::Desktop,
        "E2E owner".into(),
        profile.clone(),
        header.clone(),
    )
    .unwrap();
    enrollment
        .configure_computer_owner_device(owner.device_id)
        .unwrap();
    let create: serde_json::Value = serde_json::from_str(
        &enrollment
            .scan_computer_qr(serde_json::to_string(&prepared.pairing).unwrap())
            .unwrap(),
    )
    .unwrap();
    let created: ComputerEnrollmentCreated = post(
        &http,
        &server_origin,
        "/v1/enrollment/computers",
        &owner.device_token,
        &create,
    )
    .await;
    enrollment
        .accept_computer_created(serde_json::to_string(&created).unwrap())
        .unwrap();
    let approval: serde_json::Value = serde_json::from_str(
        &enrollment
            .approve_computer_enrollment_with_client(true, &owner_client)
            .unwrap(),
    )
    .unwrap();
    let status: ComputerEnrollmentStatus = post(
        &http,
        &server_origin,
        &format!(
            "/v1/enrollment/computers/{}/approve",
            prepared.pairing.invitation_id
        ),
        &owner.device_token,
        &approval,
    )
    .await;
    let activation: McpWorkerActivateResponse = post(
        &http,
        &server_origin,
        "/v1/mcp/device/activate",
        &owner.device_token,
        &serde_json::json!({"operation_id":prepared.operation_id,"status":status}),
    )
    .await;
    let enrollment_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(SELECT 1 FROM computer_enrollments WHERE id=$1 AND state='approved')",
    )
    .bind(prepared.pairing.invitation_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(
        enrollment_exists,
        "activation must not consume the server enrollment"
    );
    let admission: AdmissionComplete = post(
        &http,
        &server_origin,
        &format!(
            "/v1/enrollment/computers/{}/confirm",
            prepared.pairing.invitation_id
        ),
        &owner.device_token,
        &activation.confirm,
    )
    .await;
    let complete: McpDeviceStatus = post(
        &http,
        &server_origin,
        "/v1/mcp/device/complete",
        &owner.device_token,
        &serde_json::json!({"operation_id":prepared.operation_id,"admission":admission}),
    )
    .await;
    assert_eq!(complete.vault_id, vault);
    assert_eq!(
        complete.device.unwrap().device_id,
        prepared.pairing.recipient.device.device_id
    );

    let captured = owner_client
        .capture_incoming_mms(IncomingMms {
            conversation_id: None,
            sender_address: "+15551234567".into(),
            body: "private MCP integration message".into(),
            recipients: Vec::new(),
            subject: None,
            provider_message_id: Some("mcp-e2e-incoming-1".into()),
            imported: false,
            attachment_ids: Vec::new(),
        })
        .unwrap();
    let incoming_image_path = temp.path().join("incoming-mcp-image.png");
    let incoming_image_bytes = base64::engine::general_purpose::STANDARD
            .decode("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=")
            .unwrap();
    std::fs::write(&incoming_image_path, &incoming_image_bytes).unwrap();
    let incoming_image = owner_client
        .prepare_attachment(&incoming_image_path, "image/png", "incoming-mcp-image.png")
        .unwrap();
    owner_client
        .capture_incoming_mms(IncomingMms {
            conversation_id: None,
            sender_address: "+15551234567".into(),
            body: "private MCP integration image".into(),
            recipients: Vec::new(),
            subject: None,
            provider_message_id: Some("mcp-e2e-incoming-image-1".into()),
            imported: false,
            attachment_ids: vec![incoming_image.attachment_id],
        })
        .unwrap();
    peppy_gateway_simulator::drain_media(
        &owner_client,
        &server_origin,
        &owner.device_token,
        &temp.path().join("owner-media"),
    )
    .await
    .unwrap();
    peppy_gateway_simulator::upload_pending(&owner_client, &server_origin, &owner.device_token)
        .await
        .unwrap();
    let ciphertext_contains_body: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM encrypted_records WHERE vault_id=$1 AND envelope::text LIKE '%private MCP integration message%')")
        .bind(vault).fetch_one(&pool).await.unwrap();
    assert!(
        !ciphertext_contains_body,
        "server event rows must remain ciphertext"
    );
    let gateway = Uuid::new_v4();
    let gateway_token = "e2e-android-gateway-token-that-is-long-enough";
    sqlx::query("INSERT INTO devices(vault_id,device_id,role,public_key,profile_fingerprint,key_epoch,name,kind) VALUES($1,$2,'gateway','{}',$3,1,'E2E Android gateway','android')")
        .bind(vault)
        .bind(gateway)
        .bind(profile.fingerprint().unwrap())
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO device_credentials(token_digest,vault_id,device_id) VALUES($1,$2,$3)")
        .bind(peppy_server::instance::digest(gateway_token.as_bytes()).as_slice())
        .bind(vault)
        .bind(gateway)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO device_capabilities(vault_id,device_id,simulator,capabilities) VALUES($1,$2,FALSE,$3)")
        .bind(vault)
        .bind(gateway)
        .bind(serde_json::json!({"sims":[{"subscription_id":"e2e-sim","sms":"available","mms":"available","mms_content_version":peppy_client_core::MMS_CONTENT_VERSION}]}))
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("INSERT INTO device_presence(vault_id,device_id,last_seen_at) VALUES($1,$2,now())")
        .bind(vault)
        .bind(gateway)
        .execute(&pool)
        .await
        .unwrap();
    let (read_access_token, read_grant) = authorize_client(
        &http,
        &server_origin,
        &owner.device_token,
        "MCP E2E read client",
        &["conversations:read", "messages:send"],
        &["conversations:read"],
    )
    .await;
    let initialized = public_mcp(
        &http,
        &server_origin,
        &read_access_token,
        initialize_request(),
    )
    .await;
    assert_eq!(initialized["result"]["serverInfo"]["name"], "rmcp");
    let tools = public_mcp(
        &http,
        &server_origin,
        &read_access_token,
        tools_list_request(),
    )
    .await;
    assert!(
        tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "get_messages")
    );
    assert!(
        !tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "send_message")
    );
    let read = wait_for_public_messages(
        &http,
        &server_origin,
        &read_access_token,
        captured.conversation_id,
    )
    .await;
    assert!(read.to_string().contains("private MCP integration message"));
    let attachment = wait_for_public_attachment(
        &http,
        &server_origin,
        &read_access_token,
        incoming_image.attachment_id,
    )
    .await;
    assert_eq!(attachment["result"]["isError"], false);
    assert_eq!(attachment["result"]["content"][0]["type"], "image");
    assert_eq!(attachment["result"]["content"][0]["mimeType"], "image/png");
    assert_eq!(
        base64::engine::general_purpose::STANDARD
            .decode(attachment["result"]["content"][0]["data"].as_str().unwrap())
            .unwrap(),
        incoming_image_bytes
    );
    wait_for_retention_ready(&pool, vault, prepared.pairing.recipient.device.device_id).await;
    let conversations = public_mcp(
        &http,
        &server_origin,
        &read_access_token,
        tool_call_request("list_conversations", serde_json::json!({})),
    )
    .await;
    assert_eq!(conversations["result"]["isError"], false);
    assert!(conversations["result"]["content"].is_array());
    assert!(
        conversations["result"]["content"][0]["text"]
            .as_str()
            .unwrap()
            .contains(&captured.conversation_id.to_string())
    );
    let contacts = public_mcp(
        &http,
        &server_origin,
        &read_access_token,
        tool_call_request("list_contacts", serde_json::json!({})),
    )
    .await;
    assert_eq!(contacts["result"]["isError"], false);
    assert!(contacts["result"]["content"].is_array());
    let contacts_payload: serde_json::Value =
        serde_json::from_str(contacts["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(contacts_payload["books"], serde_json::json!([]));

    let send_arguments = serde_json::json!({
        "recipients":["+15557654321"],
        "text":"queued MCP integration message",
        "idempotency_key":"mcp-e2e-send-1",
        "gateway_id":gateway,
        "sim_id":"e2e-sim"
    });
    let denied_send = public_mcp(
        &http,
        &server_origin,
        &read_access_token,
        tool_call_request("send_message", send_arguments.clone()),
    )
    .await;
    assert!(
        denied_send["error"]["code"] == -32602
            && denied_send["error"]["message"] == "tool is not available",
        "read-only send succeeded"
    );

    let (send_access_token, send_grant) = authorize_client(
        &http,
        &server_origin,
        &owner.device_token,
        "MCP E2E send client",
        &["conversations:read", "messages:send"],
        &["conversations:read", "messages:send"],
    )
    .await;
    let send_tools = public_mcp(
        &http,
        &server_origin,
        &send_access_token,
        tools_list_request(),
    )
    .await;
    assert!(
        !send_tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "send_image")
    );
    let reaction = public_mcp(
        &http,
        &server_origin,
        &send_access_token,
        tool_call_request(
            "send_reaction_text",
            serde_json::json!({
                "conversation_id": captured.conversation_id,
                "target_message_id": captured.message_id,
                "text": "✓",
                "idempotency_key": "mcp-e2e-reaction-1",
                "gateway_id": gateway,
                "sim_id": "e2e-sim"
            }),
        ),
    )
    .await;
    let reaction_command_id = queued_command_id(&reaction["result"]);
    wait_for_queued_command(&pool, vault, reaction_command_id).await;
    let (media_access_token, _) = authorize_client(
        &http,
        &server_origin,
        &owner.device_token,
        "MCP E2E media client",
        &["media:send"],
        &["media:send"],
    )
    .await;
    let media_tools = public_mcp(
        &http,
        &server_origin,
        &media_access_token,
        tools_list_request(),
    )
    .await;
    assert!(
        media_tools["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "send_image"),
        "media-only grants must discover image sending"
    );
    let media_text_denied = public_mcp(
        &http,
        &server_origin,
        &media_access_token,
        tool_call_request("send_message", send_arguments.clone()),
    )
    .await;
    assert_eq!(media_text_denied["error"]["code"], -32602);
    assert_eq!(
        media_text_denied["error"]["message"],
        "tool is not available"
    );
    let command_count_before_image = queued_command_total(&pool, vault).await;
    let image = public_mcp(
        &http,
        &server_origin,
        &media_access_token,
        tool_call_request(
            "send_image",
            serde_json::json!({
                "recipients": ["+15557654321"],
                "text": "MCP image smoke",
                "image": "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=",
                "idempotency_key": "mcp-e2e-image-1",
                "gateway_id": gateway,
                "sim_id": "e2e-sim"
            }),
        ),
    )
    .await;
    let image_command_id = queued_command_id(&image["result"]);
    wait_for_queued_command(&pool, vault, image_command_id).await;
    assert_queued_command_total(&pool, vault, command_count_before_image + 1).await;
    // The queued command's transport/kind lives only in the encrypted envelope (server rows hold
    // a digest; the queue response is plaintext-free). Assert the vault classifier that gates
    // `expected_transport` for the same inputs: an attachment or a multi-recipient send is MMS.
    assert_eq!(
        owner_client.mcp_send_transport(
            &["+15557654321".to_owned()],
            "",
            &[incoming_image.attachment_id],
        ),
        Transport::Mms,
        "image send must classify as MMS"
    );
    let command_count_before_group_send = queued_command_total(&pool, vault).await;
    let multi_recipient_send = public_mcp(
        &http,
        &server_origin,
        &send_access_token,
        tool_call_request(
            "send_message",
            serde_json::json!({
                "recipients": ["+15557654321", "+15557654322"],
                "text": "two-recipient MCP smoke probe",
                "idempotency_key": "mcp-e2e-group-1",
                "gateway_id": gateway,
                "sim_id": "e2e-sim"
            }),
        ),
    )
    .await;
    let group_command_id = queued_command_id(&multi_recipient_send["result"]);
    wait_for_queued_command(&pool, vault, group_command_id).await;
    assert_queued_command_total(&pool, vault, command_count_before_group_send + 1).await;
    assert_eq!(
        owner_client.mcp_send_transport(
            &["+15557654321".to_owned(), "+15557654322".to_owned()],
            "",
            &[],
        ),
        Transport::Mms,
        "two-recipient send must classify as MMS"
    );
    let repeated_group_send = public_mcp(
        &http,
        &server_origin,
        &send_access_token,
        tool_call_request(
            "send_message",
            serde_json::json!({
                "recipients": ["+15557654321", "+15557654322"],
                "text": "two-recipient MCP smoke probe",
                "idempotency_key": "mcp-e2e-group-1",
                "gateway_id": gateway,
                "sim_id": "e2e-sim"
            }),
        ),
    )
    .await;
    assert_eq!(
        queued_command_id(&repeated_group_send["result"]),
        group_command_id,
        "two-recipient MMS send did not replay identically"
    );
    assert_queued_command_total(&pool, vault, command_count_before_group_send + 1).await;
    let command_count_before_send = queued_command_total(&pool, vault).await;
    let first_send = public_mcp(
        &http,
        &server_origin,
        &send_access_token,
        tool_call_request("send_message", send_arguments.clone()),
    )
    .await;
    let command_id = queued_command_id(&first_send["result"]);
    wait_for_queued_command(&pool, vault, command_id).await;
    assert_eq!(
        queued_command_total(&pool, vault).await,
        command_count_before_send + 1,
        "single-recipient send did not add exactly one command"
    );
    let repeated_send = public_mcp(
        &http,
        &server_origin,
        &send_access_token,
        tool_call_request("send_message", send_arguments.clone()),
    )
    .await;
    assert_eq!(queued_command_id(&repeated_send["result"]), command_id);
    assert_queued_command_total(&pool, vault, command_count_before_send + 1).await;

    worker.stop();
    worker = start_worker(
        worker_addr,
        &server_origin,
        &worker_state_dir,
        &worker_key_file,
        &token_file,
    )
    .await;
    wait_for_retention_ready(&pool, vault, prepared.pairing.recipient.device.device_id).await;
    let restarted_read = wait_for_public_messages(
        &http,
        &server_origin,
        &read_access_token,
        captured.conversation_id,
    )
    .await;
    assert!(
        restarted_read
            .to_string()
            .contains("private MCP integration message")
    );
    let restarted_send = public_mcp(
        &http,
        &server_origin,
        &send_access_token,
        tool_call_request("send_message", send_arguments),
    )
    .await;
    assert_eq!(queued_command_id(&restarted_send["result"]), command_id);
    assert_queued_command_count(&pool, vault, command_id).await;
    assert_queued_command_total(&pool, vault, command_count_before_send + 1).await;

    post_no_content(
        &http,
        &server_origin,
        &format!("/v1/mcp/grants/{send_grant}/revoke"),
        &owner.device_token,
        &(),
    )
    .await;
    let revoked_send = public_mcp_response(
        &http,
        &server_origin,
        &send_access_token,
        tools_list_request(),
    )
    .await;
    assert_eq!(revoked_send.status(), reqwest::StatusCode::UNAUTHORIZED);
    assert_ne!(
        read_grant, send_grant,
        "separate OAuth approvals must create distinct grants"
    );

    assert_worker_state_exists(&worker_state_dir, vault);
    worker.stop();
    let missing_state_dir = worker_state_dir.join("vaults").join(vault.to_string());
    std::fs::remove_dir_all(&missing_state_dir).unwrap();
    worker = start_worker(
        worker_addr,
        &server_origin,
        &worker_state_dir,
        &worker_key_file,
        &token_file,
    )
    .await;
    post_no_content(
        &http,
        &server_origin,
        &format!(
            "/v1/devices/{}/revoke",
            prepared.pairing.recipient.device.device_id
        ),
        &owner.device_token,
        &(),
    )
    .await;
    let read_after_device_revoke = public_mcp_response(
        &http,
        &server_origin,
        &read_access_token,
        tool_call_request(
            "get_messages",
            serde_json::json!({"conversation_id":captured.conversation_id}),
        ),
    )
    .await;
    assert_eq!(
        read_after_device_revoke.status(),
        reqwest::StatusCode::UNAUTHORIZED
    );
    wait_for_cleanup(
        &pool,
        vault,
        prepared.pairing.recipient.device.device_id,
        &worker_state_dir.join("vaults").join(vault.to_string()),
    )
    .await;
    let reenabled: McpEnrollmentPreparation = post(
        &http,
        &server_origin,
        "/v1/mcp/device/prepare",
        &owner.device_token,
        &(),
    )
    .await;
    assert_ne!(reenabled.operation_id, prepared.operation_id);

    server_task.abort();
    worker.stop();
    pool.close().await;
    maintenance_pool.close().await;
}

/// Drops the per-run schema on success, failure, or timeout. Drop is synchronous, so the
/// cleanup runs on a dedicated thread with its own runtime and is joined before returning.
struct SchemaGuard {
    database_url: String,
    schema: String,
}

impl Drop for SchemaGuard {
    fn drop(&mut self) {
        let database_url = self.database_url.clone();
        let schema = self.schema.clone();
        let cleanup = std::thread::spawn(move || {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("schema cleanup runtime")
                .block_on(async move {
                    let admin = PgPoolOptions::new()
                        .max_connections(1)
                        .connect(&database_url)
                        .await
                        .expect("connect for schema cleanup");
                    sqlx::query(&format!("DROP SCHEMA IF EXISTS {schema} CASCADE"))
                        .execute(&admin)
                        .await
                        .expect("drop per-run MCP schema");
                });
        });
        if cleanup.join().is_err() {
            eprintln!("per-run MCP schema cleanup failed; drop peppy_mcp_e2e_* manually");
        }
    }
}

struct WorkerProcess {
    child: std::process::Child,
}

impl WorkerProcess {
    fn stop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        self.stop();
    }
}

fn assert_worker_state_exists(worker_state_dir: &std::path::Path, vault: Uuid) {
    let vault_state_dir = worker_state_dir.join("vaults").join(vault.to_string());
    assert!(
        vault_state_dir.join("vault.sqlite").exists(),
        "worker database was not created before revocation"
    );
    assert!(
        vault_state_dir.join("custody").exists(),
        "worker custody state was not created before revocation"
    );
}

async fn start_worker(
    bind_addr: std::net::SocketAddr,
    api_url: &str,
    state_dir: &std::path::Path,
    key_file: &std::path::Path,
    token_file: &std::path::Path,
) -> WorkerProcess {
    let child = std::process::Command::new(env!("CARGO_BIN_EXE_peppy-mcp"))
        .env("PEPPY_MCP_BIND_ADDR", bind_addr.to_string())
        .env("PEPPY_MCP_API_URL", api_url)
        .env("PUBLIC_API_URL", CANONICAL_ORIGIN)
        .env("PEPPY_MCP_STATE_DIR", state_dir)
        .env("PEPPY_MCP_KEY_FILE", key_file)
        .env("PEPPY_MCP_WORKER_TOKEN_FILE", token_file)
        .spawn()
        .unwrap();
    let worker = WorkerProcess { child };
    let worker_origin = format!("http://{bind_addr}");
    let probe = reqwest::Client::builder()
        .timeout(HTTP_TIMEOUT)
        .build()
        .expect("worker health probe client");
    tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            if let Ok(response) = probe.get(format!("{worker_origin}/healthz")).send().await
                && response.status().is_success()
            {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("worker did not become healthy");
    worker
}

/// Files the worker must remove when a managed device's local state is cleaned up.
fn vault_state_files(vault_state_dir: &std::path::Path) -> Vec<std::path::PathBuf> {
    [
        "vault.sqlite",
        "vault.sqlite-shm",
        "vault.sqlite-wal",
        "vault.sqlite.media",
        "custody",
    ]
    .iter()
    .map(|name| vault_state_dir.join(name))
    .collect()
}

async fn cleanup_pending(pool: &sqlx::PgPool, vault: Uuid, device: Uuid) -> bool {
    sqlx::query_scalar("SELECT cleanup_pending FROM mcp_devices WHERE vault_id=$1 AND device_id=$2")
        .bind(vault)
        .bind(device)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn wait_for_cleanup(
    pool: &sqlx::PgPool,
    vault: Uuid,
    device: Uuid,
    vault_state_dir: &std::path::Path,
) {
    let files = vault_state_files(vault_state_dir);
    let result = tokio::time::timeout(Duration::from_secs(90), async {
        loop {
            let files_removed = files.iter().all(|path| !path.exists());
            if !cleanup_pending(pool, vault, device).await && files_removed {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await;
    if result.is_err() {
        let remaining: Vec<_> = files.into_iter().filter(|path| path.exists()).collect();
        panic!(
            "managed worker cleanup did not complete: cleanup_pending={}, remaining={remaining:?}",
            cleanup_pending(pool, vault, device).await
        );
    }
}

async fn authorize_client(
    http: &reqwest::Client,
    server: &str,
    owner_token: &str,
    client_name: &str,
    requested_scopes: &[&str],
    approved_scopes: &[&str],
) -> (String, Uuid) {
    let registration = http
        .post(format!("{server}/oauth/register"))
        .json(&serde_json::json!({
            "client_name": client_name,
            "redirect_uris": ["https://client.example/callback"],
            "scope": requested_scopes.join(" "),
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
            "application_type": "web",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(registration.status(), reqwest::StatusCode::CREATED);
    let client_id = registration.json::<serde_json::Value>().await.unwrap()["client_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let verifier = "mcp-e2e-pkce-verifier-long-enough-for-rfc7636-validation";
    let resource = format!("{CANONICAL_ORIGIN}/mcp");
    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    let mut authorize = Url::parse(&format!("{server}/oauth/authorize")).unwrap();
    authorize
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &client_id)
        .append_pair("redirect_uri", "https://client.example/callback")
        .append_pair("state", "mcp-e2e-state")
        .append_pair("scope", &requested_scopes.join(" "))
        .append_pair("resource", &resource)
        .append_pair("code_challenge", &challenge)
        .append_pair("code_challenge_method", "S256");
    let no_redirects = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(HTTP_TIMEOUT)
        .build()
        .unwrap();
    let authorization = no_redirects.get(authorize).send().await.unwrap();
    assert_eq!(
        authorization.status(),
        reqwest::StatusCode::TEMPORARY_REDIRECT
    );
    let location = authorization.headers()[reqwest::header::LOCATION]
        .to_str()
        .unwrap();
    let authorization_id = Url::parse(location)
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "mcp_authorize")
        .unwrap()
        .1
        .into_owned();
    let view: serde_json::Value = get_authenticated(
        http,
        server,
        &format!("/v1/mcp/authorizations/{authorization_id}"),
        owner_token,
    )
    .await;
    assert_eq!(view["requestedScopes"], serde_json::json!(requested_scopes));
    let decision: serde_json::Value = post(
        http,
        server,
        &format!("/v1/mcp/authorizations/{authorization_id}/approve"),
        owner_token,
        &serde_json::json!({"scopes": approved_scopes}),
    )
    .await;
    let code = Url::parse(decision["redirectUri"].as_str().unwrap())
        .unwrap()
        .query_pairs()
        .find(|(key, _)| key == "code")
        .unwrap()
        .1
        .into_owned();
    let tokens = http
        .post(format!("{server}/oauth/token"))
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client_id.as_str()),
            ("code", code.as_str()),
            ("redirect_uri", "https://client.example/callback"),
            ("resource", resource.as_str()),
            ("code_verifier", verifier),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(tokens.status(), reqwest::StatusCode::OK);
    let tokens = tokens.json::<serde_json::Value>().await.unwrap();
    assert_eq!(tokens["scope"], approved_scopes.join(" "));
    let access_token = tokens["access_token"].as_str().unwrap().to_owned();
    let grants: Vec<McpGrantView> =
        get_authenticated(http, server, "/v1/mcp/grants", owner_token).await;
    let grant = grants
        .into_iter()
        .find(|grant| {
            grant.client_name == client_name
                && grant
                    .scopes
                    .iter()
                    .map(ToString::to_string)
                    .eq(approved_scopes.iter().map(|scope| scope.to_string()))
        })
        .expect("OAuth approval created the expected grant")
        .id;
    (access_token, grant)
}

async fn get_authenticated<T: serde::de::DeserializeOwned>(
    http: &reqwest::Client,
    server: &str,
    path: &str,
    token: &str,
) -> T {
    let response = http
        .get(format!("{server}{path}"))
        .header("origin", CANONICAL_ORIGIN)
        .header("peppy-instance-session", INSTANCE_SESSION)
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(status.is_success(), "{path}: {status} {body}");
    serde_json::from_str(&body).unwrap()
}

fn initialize_request() -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"mcp-e2e","version":"1"}}})
}

fn tools_list_request() -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}})
}

fn tool_call_request(name: &str, arguments: serde_json::Value) -> serde_json::Value {
    serde_json::json!({"jsonrpc":"2.0","id":3,"method":"tools/call","params":{"name":name,"arguments":arguments}})
}

async fn public_mcp(
    http: &reqwest::Client,
    server: &str,
    access_token: &str,
    request: serde_json::Value,
) -> serde_json::Value {
    let response = public_mcp_response(http, server, access_token, request).await;
    let status = response.status();
    let body = response.text().await.unwrap();
    assert!(status.is_success(), "public MCP: {status} {body}");
    serde_json::from_str(&body).unwrap()
}

async fn public_mcp_response(
    http: &reqwest::Client,
    server: &str,
    access_token: &str,
    request: serde_json::Value,
) -> reqwest::Response {
    http.post(format!("{server}/mcp"))
        .header("host", "mcp-e2e.peppy.test")
        .header("origin", CANONICAL_ORIGIN)
        .header("accept", "application/json, text/event-stream")
        .header("mcp-protocol-version", "2025-03-26")
        .bearer_auth(access_token)
        .json(&request)
        .send()
        .await
        .unwrap()
}

fn queued_command_id(response: &serde_json::Value) -> Uuid {
    assert_eq!(response["isError"], false, "send failed: {response}");
    let queued: serde_json::Value = serde_json::from_str(
        response["content"][0]["text"]
            .as_str()
            .expect("queued send text result"),
    )
    .unwrap();
    assert_eq!(queued["status"], "queued", "send was not queued: {queued}");
    Uuid::parse_str(
        queued["command_id"]
            .as_str()
            .expect("queued send command id"),
    )
    .unwrap()
}

/// Server command-row count and encrypted command-envelope count for one command ID.
async fn queued_command_rows(pool: &sqlx::PgPool, vault: Uuid, command: Uuid) -> (i64, i64) {
    sqlx::query_as(
        "SELECT \
            (SELECT count(*) FROM commands WHERE vault_id=$1 AND command_id=$2), \
            (SELECT count(*) FROM encrypted_records records JOIN event_log log ON log.vault_id=records.vault_id AND log.envelope_id=records.envelope_id WHERE log.vault_id=$1 AND log.command_id=$2 AND log.purpose='command')",
    )
    .bind(vault)
    .bind(command)
    .fetch_one(pool)
    .await
    .unwrap()
}

async fn wait_for_queued_command(pool: &sqlx::PgPool, vault: Uuid, command: Uuid) {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if queued_command_rows(pool, vault, command).await == (1, 1) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("queued command was not uploaded exactly once");
}

async fn assert_queued_command_count(pool: &sqlx::PgPool, vault: Uuid, command: Uuid) {
    let (commands, encrypted_outbox) = queued_command_rows(pool, vault, command).await;
    assert_eq!(commands, 1, "command duplicated after worker restart");
    assert_eq!(encrypted_outbox, 1, "encrypted command envelope duplicated");
}

async fn queued_command_total(pool: &sqlx::PgPool, vault: Uuid) -> i64 {
    sqlx::query_scalar("SELECT count(*) FROM commands WHERE vault_id=$1")
        .bind(vault)
        .fetch_one(pool)
        .await
        .unwrap()
}

async fn assert_queued_command_total(pool: &sqlx::PgPool, vault: Uuid, expected: i64) {
    assert_eq!(
        queued_command_total(pool, vault).await,
        expected,
        "the fixture command total changed across retry or restart"
    );
}

async fn wait_for_retention_ready(pool: &sqlx::PgPool, vault: Uuid, device: Uuid) {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let readiness: Option<(bool, bool)> = sqlx::query_as(
                "SELECT cleanup_ready,reconciliation_needed FROM attachment_retention_devices WHERE vault_id=$1 AND device_id=$2",
            )
            .bind(vault)
            .bind(device)
            .fetch_optional(pool)
            .await
            .unwrap();
            if readiness == Some((true, false)) {
                return;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("worker did not publish retention readiness");
}

async fn wait_for_public_messages(
    http: &reqwest::Client,
    server: &str,
    access_token: &str,
    conversation: peppy_client_core::ConversationId,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let value = public_mcp(
                http,
                server,
                access_token,
                tool_call_request(
                    "get_messages",
                    serde_json::json!({"conversation_id":conversation}),
                ),
            )
            .await;
            if value
                .to_string()
                .contains("private MCP integration message")
            {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("worker did not hydrate owner event")
}

async fn wait_for_public_attachment(
    http: &reqwest::Client,
    server: &str,
    access_token: &str,
    attachment: peppy_client_core::AttachmentId,
) -> serde_json::Value {
    tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            let value = public_mcp(
                http,
                server,
                access_token,
                tool_call_request(
                    "get_attachment",
                    serde_json::json!({"attachment_id": attachment}),
                ),
            )
            .await;
            if value["result"]["isError"] == false {
                return value;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .expect("worker did not hydrate owner attachment")
}

async fn post<T: serde::de::DeserializeOwned, B: serde::Serialize>(
    http: &reqwest::Client,
    origin: &str,
    path: &str,
    token: &str,
    body: &B,
) -> T {
    let response = http
        .post(format!("{origin}{path}"))
        .header("origin", CANONICAL_ORIGIN)
        .header("peppy-instance-session", INSTANCE_SESSION)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert!(status.is_success(), "{path}: {status} {text}");
    serde_json::from_str(&text).unwrap()
}

async fn post_no_content<B: serde::Serialize>(
    http: &reqwest::Client,
    origin: &str,
    path: &str,
    token: &str,
    body: &B,
) {
    let response = http
        .post(format!("{origin}{path}"))
        .header("origin", CANONICAL_ORIGIN)
        .header("peppy-instance-session", INSTANCE_SESSION)
        .bearer_auth(token)
        .json(body)
        .send()
        .await
        .unwrap();
    let status = response.status();
    let text = response.text().await.unwrap();
    assert_eq!(status, reqwest::StatusCode::NO_CONTENT, "{path}: {text}");
}
