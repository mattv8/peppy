use std::net::IpAddr;

use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::{IntoResponse, Redirect, Response},
    routing::{get, post},
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use peppy_hosted_api::mcp::{McpAuthorizationDecision, McpAuthorizationView, McpScope};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;

use super::super::{
    AccessPrincipal, ApiError, ApiResult, ApiState, Operation, PeerAddress, admission,
    authenticated, database_unavailable, owner, revalidate_principal, scope,
};

const AUTHORIZATION_TTL_SECONDS: i64 = 600;
const CODE_TTL_SECONDS: i64 = 120;
const ACCESS_TTL_SECONDS: i64 = 900;
const REFRESH_TTL_SECONDS: i64 = 2_592_000;
const OAUTH_SOURCE_LIMIT: u8 = 10;
const OAUTH_GLOBAL_LIMIT: u8 = 100;
const REGISTRATION_CAPACITY_LOCK: i64 = 2_481_843_414;
const AUTHORIZATION_CAPACITY_LOCK: i64 = 2_481_843_415;

#[derive(Clone)]
pub(super) struct McpPrincipal {
    pub vault_id: Uuid,
    pub device_id: Uuid,
    pub grant_id: Uuid,
    pub scopes: Vec<McpScope>,
    token_digest: Vec<u8>,
    family_id: Uuid,
    resource: String,
    pub(super) device_role: String,
}

impl McpPrincipal {
    /// This identity is resolved from the live MCP device record; it never
    /// contains the bearer token.
    pub(super) fn access_principal(&self) -> AccessPrincipal {
        AccessPrincipal {
            vault_id: self.vault_id,
            device_id: self.device_id,
            role: self.device_role.clone(),
        }
    }
}

pub(super) fn routes() -> Router<ApiState> {
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-authorization-server",
            get(authorization_server_metadata),
        )
        .route("/oauth/register", post(register))
        .route("/oauth/authorize", get(authorize))
        .route("/oauth/token", post(token))
        .route("/oauth/revoke", post(revoke))
        .route("/v1/mcp/authorizations/{id}", get(authorization_view))
        .route("/v1/mcp/authorizations/{id}/approve", post(approve))
        .route("/v1/mcp/authorizations/{id}/deny", post(deny))
}

fn origin(state: &ApiState) -> ApiResult<&url::Url> {
    state.instance_origin.as_ref().ok_or(ApiError(
        StatusCode::SERVICE_UNAVAILABLE,
        "mcp_origin_unavailable",
    ))
}

fn resource(state: &ApiState) -> ApiResult<String> {
    origin(state)?
        .join("mcp")
        .map(|url| url.to_string())
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "mcp_origin_unavailable"))
}

async fn protected_resource_metadata(
    State(state): State<ApiState>,
) -> ApiResult<Json<serde_json::Value>> {
    let resource = resource(&state)?;
    Ok(Json(serde_json::json!({
        "resource": resource,
        "authorization_servers": [origin(&state)?.as_str()],
        "scopes_supported": scope_names(),
    })))
}

async fn authorization_server_metadata(
    State(state): State<ApiState>,
) -> ApiResult<Json<serde_json::Value>> {
    let origin = origin(&state)?.as_str();
    Ok(Json(serde_json::json!({
        "issuer": origin,
        "authorization_endpoint": format!("{origin}oauth/authorize"),
        "token_endpoint": format!("{origin}oauth/token"),
        "registration_endpoint": format!("{origin}oauth/register"),
        "revocation_endpoint": format!("{origin}oauth/revoke"),
        "response_types_supported": ["code"],
        "grant_types_supported": ["authorization_code", "refresh_token"],
        "token_endpoint_auth_methods_supported": ["none"],
        "code_challenge_methods_supported": ["S256"],
        "scopes_supported": scope_names(),
    })))
}

#[derive(Deserialize)]
struct Registration {
    client_name: String,
    redirect_uris: Vec<String>,
    #[serde(default)]
    scope: Option<String>,
    #[serde(default)]
    grant_types: Option<Vec<String>>,
    #[serde(default)]
    response_types: Option<Vec<String>>,
    #[serde(default)]
    token_endpoint_auth_method: Option<String>,
    #[serde(flatten)]
    extensions: std::collections::BTreeMap<String, serde_json::Value>,
}

async fn register(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    request: Result<Json<Registration>, axum::extract::rejection::JsonRejection>,
) -> Response {
    let Ok(Json(request)) = request else {
        return oauth_error(ApiError(StatusCode::BAD_REQUEST, "invalid_client_metadata"));
    };
    match register_inner(&state, peer, request).await {
        Ok(value) => no_store((StatusCode::CREATED, value).into_response()),
        Err(error) => oauth_error(error),
    }
}

async fn register_inner(
    state: &ApiState,
    peer: Option<IpAddr>,
    request: Registration,
) -> ApiResult<Json<serde_json::Value>> {
    let grant_types = request
        .grant_types
        .clone()
        .unwrap_or_else(|| vec!["authorization_code".into(), "refresh_token".into()]);
    let response_types = request
        .response_types
        .clone()
        .unwrap_or_else(|| vec!["code".into()]);
    let token_endpoint_auth_method = request
        .token_endpoint_auth_method
        .clone()
        .unwrap_or_else(|| "none".into());
    if request.client_name.trim().is_empty()
        || request.client_name.len() > 128
        || request.redirect_uris.is_empty()
        || request.redirect_uris.len() > 8
        || serde_json::to_vec(&request.extensions).map_or(true, |value| value.len() > 8 * 1024)
        || (grant_types != ["authorization_code", "refresh_token"]
            && grant_types != ["authorization_code"])
        || response_types != ["code"]
        || token_endpoint_auth_method != "none"
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_client_metadata"));
    }
    let redirects = request
        .redirect_uris
        .into_iter()
        .map(|value| {
            if value.len() > 2_048 {
                return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_redirect_uri"));
            }
            validate_redirect(&value).map(|_| value)
        })
        .collect::<ApiResult<Vec<_>>>()?;
    let scopes = parse_scopes(request.scope.as_deref().unwrap_or("conversations:read"))
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_client_metadata"))?;
    oauth_admission(
        state,
        peer,
        b"mcp-oauth-register",
        "mcp_oauth_registration_limited",
    )
    .await?;
    sweep_expired_authorizations(state).await?;
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_register_begin"))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(REGISTRATION_CAPACITY_LOCK)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_register_capacity_lock"))?;
    let client_count: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_oauth_clients")
        .fetch_one(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_client_cap"))?;
    if client_count >= 1024 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "mcp_oauth_registration_limited",
        ));
    }
    let client_id = opaque_secret();
    sqlx::query("INSERT INTO mcp_oauth_clients(client_id,client_name,redirect_uris,allowed_scopes) VALUES($1,$2,$3,$4)")
        .bind(&client_id).bind(request.client_name.trim()).bind(&redirects).bind(scope_strings(&scopes))
        .execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_register"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_register_commit"))?;
    Ok(Json(serde_json::json!({
        "client_id": client_id,
        "client_name": request.client_name.trim(),
        "redirect_uris": redirects,
        "scope": scope_strings(&scopes).join(" "),
        "grant_types": grant_types,
        "response_types": response_types,
        "token_endpoint_auth_method": token_endpoint_auth_method,
    })))
}

#[derive(Deserialize)]
struct AuthorizeQuery {
    response_type: String,
    client_id: String,
    redirect_uri: String,
    state: String,
    scope: String,
    resource: String,
    code_challenge: String,
    code_challenge_method: String,
}

async fn authorize(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    Query(query): Query<AuthorizeQuery>,
) -> ApiResult<Redirect> {
    if query.response_type != "code"
        || query.state.len() > 2048
        || query.code_challenge_method != "S256"
        || !valid_pkce(&query.code_challenge)
        || query.resource != resource(&state)?
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_authorization_request",
        ));
    }
    validate_redirect(&query.redirect_uri)?;
    let scopes = parse_scopes(&query.scope)?;
    let client = sqlx::query(
        "SELECT redirect_uris,allowed_scopes FROM mcp_oauth_clients WHERE client_id=$1",
    )
    .bind(&query.client_id)
    .fetch_optional(&state.db)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_client"))?
    .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_client"))?;
    if !client
        .get::<Vec<String>, _>("redirect_uris")
        .contains(&query.redirect_uri)
        || !scope_strings(&scopes).iter().all(|scope| {
            client
                .get::<Vec<String>, _>("allowed_scopes")
                .contains(scope)
        })
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid_authorization_request",
        ));
    }
    oauth_admission(
        &state,
        peer,
        b"mcp-oauth-authorize",
        "mcp_oauth_request_limited",
    )
    .await?;
    sweep_expired_authorizations(&state).await?;
    let id = opaque_secret();
    let mut tx = state
        .db
        .begin()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_authorize_begin"))?;
    sqlx::query("SELECT pg_advisory_xact_lock($1)")
        .bind(AUTHORIZATION_CAPACITY_LOCK)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_authorize_capacity_lock"))?;
    let pending_count: i64 = sqlx::query_scalar("SELECT count(*) FROM mcp_oauth_authorizations WHERE expires_at>now() AND decided_at IS NULL AND denied_at IS NULL")
        .fetch_one(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_request_cap"))?;
    if pending_count >= 4096 {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "mcp_oauth_request_limited",
        ));
    }
    sqlx::query("INSERT INTO mcp_oauth_authorizations(id,client_id,redirect_uri,state,code_challenge,requested_scopes,resource,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,now()+($8::text||' seconds')::interval)")
        .bind(&id).bind(query.client_id).bind(query.redirect_uri).bind(query.state).bind(query.code_challenge).bind(scope_strings(&scopes)).bind(query.resource).bind(AUTHORIZATION_TTL_SECONDS)
        .execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_authorize"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_authorize_commit"))?;
    let consent_origin = state
        .mcp_consent_origin
        .as_deref()
        .unwrap_or(origin(&state)?.as_str());
    let location = url::Url::parse(consent_origin)
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "mcp_consent_unavailable"))?
        .join(&format!("mcp/authorize?mcp_authorize={id}"))
        .map_err(|_| ApiError(StatusCode::SERVICE_UNAVAILABLE, "mcp_consent_unavailable"))?;
    Ok(Redirect::temporary(location.as_str()))
}

async fn sweep_expired_authorizations(state: &ApiState) -> ApiResult<()> {
    sqlx::query("SELECT mcp_sweep_oauth()")
        .execute(&state.db)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_sweep"))?;
    Ok(())
}

async fn oauth_admission(
    state: &ApiState,
    peer: Option<IpAddr>,
    operation: &[u8],
    code: &'static str,
) -> ApiResult<()> {
    let mut admissions = state.mcp_oauth_admissions.lock().await;
    if !admissions.admit_with_limit(
        admission::source_digest(operation, peer),
        OAUTH_SOURCE_LIMIT,
    ) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, code));
    }
    if !admissions.admit_with_limit(
        admission::source_digest(&[operation, b":global"].concat(), None),
        OAUTH_GLOBAL_LIMIT,
    ) {
        return Err(ApiError(StatusCode::TOO_MANY_REQUESTS, code));
    }
    Ok(())
}

async fn authorization_view(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<McpAuthorizationView>> {
    let principal = authenticated(&state, &headers, Operation::Read).await?;
    owner(&principal)?;
    let mut tx = scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_authorization_view_begin"))?;
    if revalidate_principal(&mut tx, &principal).await? != "owner" {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_required"));
    }
    let row = sqlx::query("SELECT a.requested_scopes,a.resource,a.redirect_uri,c.client_id,c.client_name FROM mcp_oauth_authorizations a JOIN mcp_oauth_clients c ON c.client_id=a.client_id WHERE a.id=$1 AND a.expires_at>now() AND a.decided_at IS NULL AND a.denied_at IS NULL")
        .bind(&id).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_authorization_lookup"))?.ok_or(ApiError(StatusCode::NOT_FOUND, "mcp_authorization_not_found"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_authorization_view_commit"))?;
    let redirect = validate_redirect(&row.get::<String, _>("redirect_uri"))?;
    Ok(Json(McpAuthorizationView {
        id,
        client_name: row.get("client_name"),
        client_id: row.get("client_id"),
        requested_scopes: scopes_from_strings(row.get("requested_scopes"))?,
        server_origin: origin(&state)?.origin().ascii_serialization(),
        resource: row.get("resource"),
        redirect_origin: redirect.origin().ascii_serialization(),
        redirect_is_loopback: is_loopback_redirect(&redirect),
    }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Decision {
    scopes: Vec<McpScope>,
}

async fn approve(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(decision): Json<Decision>,
) -> ApiResult<Json<McpAuthorizationDecision>> {
    let principal = authenticated(&state, &headers, Operation::Revoke).await?;
    owner(&principal)?;
    let selected = decision.scopes;
    if !McpScope::is_valid_set(&selected) {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_scopes"));
    }
    let mut tx = scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_approve_begin"))?;
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(principal.vault)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_approve_vault_lock"))?;
    if revalidate_principal(&mut tx, &principal).await? != "owner" {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_required"));
    }
    let row = sqlx::query("SELECT a.client_id,a.redirect_uri,a.state,a.code_challenge,a.resource,a.requested_scopes,c.client_name FROM mcp_oauth_authorizations a JOIN mcp_oauth_clients c ON c.client_id=a.client_id WHERE a.id=$1 AND a.expires_at>now() AND a.decided_at IS NULL AND a.denied_at IS NULL FOR UPDATE")
        .bind(&id).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_approve_lookup"))?.ok_or(ApiError(StatusCode::NOT_FOUND, "mcp_authorization_not_found"))?;
    let requested: Vec<String> = row.get("requested_scopes");
    if !scope_strings(&selected)
        .iter()
        .all(|scope| requested.contains(scope))
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_scopes"));
    }
    let mcp_device_id: Uuid = sqlx::query_scalar("SELECT m.device_id FROM mcp_devices m JOIN devices d ON d.vault_id=m.vault_id AND d.device_id=m.device_id WHERE m.vault_id=$1 AND d.revoked_at IS NULL ORDER BY m.created_at DESC LIMIT 1")
        .bind(principal.vault).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_device"))?.ok_or(ApiError(StatusCode::CONFLICT, "mcp_device_unavailable"))?;
    let grant_id = Uuid::new_v4();
    sqlx::query(
        "INSERT INTO mcp_grants(id,vault_id,device_id,client_name,scopes) VALUES($1,$2,$3,$4,$5)",
    )
    .bind(grant_id)
    .bind(principal.vault)
    .bind(mcp_device_id)
    .bind(row.get::<String, _>("client_name"))
    .bind(scope_strings(&selected))
    .execute(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_grant"))?;
    let code = opaque_secret();
    sqlx::query("INSERT INTO mcp_oauth_codes(digest,vault_id,device_id,grant_id,client_id,redirect_uri,code_challenge,resource,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,now()+($9::text||' seconds')::interval)")
        .bind(digest(&code)).bind(principal.vault).bind(mcp_device_id).bind(grant_id).bind(row.get::<String,_>("client_id")).bind(row.get::<String,_>("redirect_uri")).bind(row.get::<String,_>("code_challenge")).bind(row.get::<String,_>("resource")).bind(CODE_TTL_SECONDS).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_code"))?;
    sqlx::query(
        "UPDATE mcp_oauth_authorizations SET decided_at=now(),vault_id=$2,device_id=$3 WHERE id=$1",
    )
    .bind(&id)
    .bind(principal.vault)
    .bind(principal.device)
    .execute(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_decide"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_approve_commit"))?;
    let mut redirect = url::Url::parse(&row.get::<String, _>("redirect_uri"))
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "mcp_redirect_invalid"))?;
    redirect
        .query_pairs_mut()
        .append_pair("code", &code)
        .append_pair("state", &row.get::<String, _>("state"));
    Ok(Json(McpAuthorizationDecision {
        redirect_uri: redirect.to_string(),
    }))
}

async fn deny(
    State(state): State<ApiState>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> ApiResult<Json<McpAuthorizationDecision>> {
    let principal = authenticated(&state, &headers, Operation::Revoke).await?;
    owner(&principal)?;
    let mut tx = scope::begin(&state.db, principal.vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_deny_begin"))?;
    sqlx::query("SELECT 1 FROM vaults WHERE vault_id=$1 FOR UPDATE")
        .bind(principal.vault)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_deny_vault_lock"))?;
    if revalidate_principal(&mut tx, &principal).await? != "owner" {
        return Err(ApiError(StatusCode::FORBIDDEN, "owner_required"));
    }
    let row = sqlx::query("UPDATE mcp_oauth_authorizations SET denied_at=now(),vault_id=$2,device_id=$3 WHERE id=$1 AND expires_at>now() AND decided_at IS NULL AND denied_at IS NULL RETURNING redirect_uri,state")
        .bind(&id).bind(principal.vault).bind(principal.device).fetch_optional(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_deny"))?.ok_or(ApiError(StatusCode::NOT_FOUND, "mcp_authorization_not_found"))?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_deny_commit"))?;
    let mut redirect = url::Url::parse(&row.get::<String, _>("redirect_uri"))
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "mcp_redirect_invalid"))?;
    redirect
        .query_pairs_mut()
        .append_pair("error", "access_denied")
        .append_pair("state", &row.get::<String, _>("state"));
    Ok(Json(McpAuthorizationDecision {
        redirect_uri: redirect.to_string(),
    }))
}

#[derive(Deserialize)]
struct TokenForm {
    grant_type: Option<String>,
    code: Option<String>,
    redirect_uri: Option<String>,
    client_id: Option<String>,
    code_verifier: Option<String>,
    refresh_token: Option<String>,
    resource: Option<String>,
}

async fn token(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    form: Result<axum::extract::Form<TokenForm>, axum::extract::rejection::FormRejection>,
) -> Response {
    let Ok(axum::extract::Form(form)) = form else {
        return oauth_error(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    };
    let result = match required(&form.grant_type).map(str::to_owned) {
        Ok(grant_type) if grant_type == "authorization_code" => {
            match oauth_admission(&state, peer, b"mcp-oauth-token", "mcp_oauth_token_limited").await
            {
                Ok(()) => exchange_code(&state, form).await,
                Err(error) => Err(error),
            }
        }
        Ok(grant_type) if grant_type == "refresh_token" => {
            match oauth_admission(&state, peer, b"mcp-oauth-token", "mcp_oauth_token_limited").await
            {
                Ok(()) => refresh(&state, form).await,
                Err(error) => Err(error),
            }
        }
        Ok(_) => Err(ApiError(StatusCode::BAD_REQUEST, "unsupported_grant_type")),
        Err(error) => Err(error),
    };
    match result {
        Ok(value) => no_store((StatusCode::OK, Json(value)).into_response()),
        Err(error) => oauth_error(error),
    }
}

async fn exchange_code(state: &ApiState, form: TokenForm) -> ApiResult<serde_json::Value> {
    let code = required(&form.code)?;
    let verifier = required(&form.code_verifier)?;
    let redirect_uri = required(&form.redirect_uri)?;
    let client_id = required(&form.client_id)?;
    let resource = required(&form.resource)?;
    require_client(state, client_id).await?;
    let vault: Uuid = sqlx::query_scalar("SELECT vault_id FROM mcp_resolve_code($1)")
        .bind(digest(code))
        .fetch_optional(&state.db)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_code_resolve"))?
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_grant"))?;
    let mut tx = scope::begin(&state.db, vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_token_begin"))?;
    let row = sqlx::query(
        "SELECT c.*,g.scopes,c.used_at IS NOT NULL AS used FROM mcp_oauth_codes c JOIN mcp_grants g ON g.id=c.grant_id AND g.revoked_at IS NULL JOIN devices d ON d.vault_id=c.vault_id AND d.device_id=c.device_id AND d.revoked_at IS NULL WHERE c.digest=$1 FOR UPDATE OF c,g,d",
    )
    .bind(digest(code))
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_code_lock"))?
    .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_grant"))?;
    let unexpired: bool = sqlx::query_scalar(
        "SELECT expires_at > clock_timestamp() FROM mcp_oauth_codes WHERE digest=$1",
    )
    .bind(digest(code))
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_code_expiry"))?;
    let expected = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    if row.get::<String, _>("client_id") != client_id
        || redirect_uri != row.get::<String, _>("redirect_uri")
        || row.get::<String, _>("code_challenge") != expected
        || resource != row.get::<String, _>("resource")
        || row.get::<bool, _>("used")
        || !unexpired
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_grant"));
    }
    sqlx::query("UPDATE mcp_oauth_codes SET used_at=now() WHERE digest=$1")
        .bind(digest(code))
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_code_use"))?;
    let value = issue_tokens(&mut tx, &row, None).await?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_token_commit"))?;
    Ok(value)
}

async fn refresh(state: &ApiState, form: TokenForm) -> ApiResult<serde_json::Value> {
    let token = required(&form.refresh_token)?;
    let client_id = required(&form.client_id)?;
    let resource = required(&form.resource)?;
    require_client(state, client_id).await?;
    let vault: Uuid = sqlx::query_scalar("SELECT vault_id FROM mcp_resolve_refresh_token($1)")
        .bind(digest(token))
        .fetch_optional(&state.db)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_resolve"))?
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_grant"))?;
    let mut tx = scope::begin(&state.db, vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_begin"))?;
    let family: Uuid = sqlx::query_scalar(
        "SELECT family_id FROM mcp_oauth_tokens WHERE digest=$1 AND token_kind='refresh'",
    )
    .bind(digest(token))
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_family"))?
    .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_grant"))?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(family.to_string())
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_family_lock"))?;
    let row = sqlx::query(
        "SELECT t.*,g.scopes,t.used_at IS NOT NULL AS used,t.revoked_at IS NOT NULL AS revoked FROM mcp_oauth_tokens t JOIN mcp_grants g ON g.id=t.grant_id AND g.revoked_at IS NULL JOIN devices d ON d.vault_id=t.vault_id AND d.device_id=t.device_id AND d.revoked_at IS NULL WHERE t.digest=$1 AND t.token_kind='refresh' FOR UPDATE OF t,g,d",
    )
    .bind(digest(token))
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_lock"))?
    .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_grant"))?;
    let unexpired: bool = sqlx::query_scalar(
        "SELECT expires_at > clock_timestamp() FROM mcp_oauth_tokens WHERE digest=$1",
    )
    .bind(digest(token))
    .fetch_one(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_expiry"))?;
    if row.get::<String, _>("client_id") != client_id
        || row.get::<String, _>("resource") != resource
        || row.get::<bool, _>("used")
        || row.get::<bool, _>("revoked")
        || !unexpired
    {
        sqlx::query("UPDATE mcp_oauth_tokens SET revoked_at=now() WHERE family_id=$1 AND revoked_at IS NULL").bind(row.get::<Uuid,_>("family_id")).execute(&mut *tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_revoke"))?;
        tx.commit()
            .await
            .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_revoke_commit"))?;
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_grant"));
    }
    sqlx::query("UPDATE mcp_oauth_tokens SET used_at=now() WHERE digest=$1")
        .bind(digest(token))
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_use"))?;
    let value = issue_tokens(&mut tx, &row, Some(row.get("family_id"))).await?;
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_refresh_commit"))?;
    Ok(value)
}

async fn issue_tokens(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    row: &sqlx::postgres::PgRow,
    family: Option<Uuid>,
) -> ApiResult<serde_json::Value> {
    let access = opaque_secret();
    let refresh = opaque_secret();
    let family = family.unwrap_or_else(Uuid::new_v4);
    for (secret, kind, ttl) in [
        (&access, "access", ACCESS_TTL_SECONDS),
        (&refresh, "refresh", REFRESH_TTL_SECONDS),
    ] {
        sqlx::query("INSERT INTO mcp_oauth_tokens(digest,token_kind,family_id,vault_id,device_id,grant_id,client_id,resource,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,$8,clock_timestamp()+($9::text||' seconds')::interval)")
            .bind(digest(secret)).bind(kind).bind(family).bind(row.get::<Uuid,_>("vault_id")).bind(row.get::<Uuid,_>("device_id")).bind(row.get::<Uuid,_>("grant_id")).bind(row.get::<String,_>("client_id")).bind(row.get::<String,_>("resource")).bind(ttl).execute(&mut **tx).await.map_err(|error| database_unavailable(&error, "mcp_oauth_issue"))?;
    }
    let scope = scope_strings(&scopes_from_strings(row.get("scopes"))?).join(" ");
    Ok(
        serde_json::json!({"access_token":access,"token_type":"Bearer","expires_in":ACCESS_TTL_SECONDS,"refresh_token":refresh,"resource":row.get::<String,_>("resource"),"scope":scope}),
    )
}

#[derive(Deserialize)]
struct RevokeForm {
    token: String,
    client_id: Option<String>,
}

async fn revoke(
    State(state): State<ApiState>,
    PeerAddress(peer): PeerAddress,
    form: Result<axum::extract::Form<RevokeForm>, axum::extract::rejection::FormRejection>,
) -> Response {
    let Ok(axum::extract::Form(form)) = form else {
        return oauth_error(ApiError(StatusCode::BAD_REQUEST, "invalid_request"));
    };
    let result = match oauth_admission(
        &state,
        peer,
        b"mcp-oauth-revoke",
        "mcp_oauth_revoke_limited",
    )
    .await
    {
        Ok(()) => revoke_inner(&state, form).await,
        Err(error) => Err(error),
    };
    match result {
        Ok(response) => response,
        Err(error) => oauth_error(error),
    }
}

async fn revoke_inner(state: &ApiState, form: RevokeForm) -> ApiResult<Response> {
    let client_id = required(&form.client_id)?;
    let digest = digest(&form.token);
    let Some(vault) = sqlx::query_scalar("SELECT vault_id FROM mcp_resolve_token($1)")
        .bind(&digest)
        .fetch_optional(&state.db)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_revoke_resolve"))?
    else {
        return Ok(no_store(StatusCode::OK.into_response()));
    };
    let mut tx = scope::begin(&state.db, vault)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_revoke_begin"))?;
    let Some((_token_client, token_kind, family_id)) = sqlx::query_as::<_, (String, String, Uuid)>(
        "SELECT client_id,token_kind,family_id FROM mcp_oauth_tokens WHERE digest=$1",
    )
    .bind(&digest)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_revoke_lock"))?
    else {
        return Ok(no_store(StatusCode::OK.into_response()));
    };
    if token_kind == "refresh" {
        sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
            .bind(family_id.to_string())
            .execute(&mut *tx)
            .await
            .map_err(|error| database_unavailable(&error, "mcp_oauth_revoke_family_lock"))?;
    }
    let Some(token_client) = sqlx::query_scalar::<_, String>(
        "SELECT client_id FROM mcp_oauth_tokens WHERE digest=$1 FOR UPDATE",
    )
    .bind(&digest)
    .fetch_optional(&mut *tx)
    .await
    .map_err(|error| database_unavailable(&error, "mcp_oauth_revoke_lock"))?
    else {
        return Ok(no_store(StatusCode::OK.into_response()));
    };
    if token_client != client_id {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_client"));
    }
    if token_kind == "refresh" {
        sqlx::query("UPDATE mcp_oauth_tokens SET revoked_at=now() WHERE family_id=$1 AND revoked_at IS NULL")
            .bind(family_id)
            .execute(&mut *tx)
            .await
            .map_err(|error| database_unavailable(&error, "mcp_oauth_revoke"))?;
    } else {
        sqlx::query(
            "UPDATE mcp_oauth_tokens SET revoked_at=now() WHERE digest=$1 AND revoked_at IS NULL",
        )
        .bind(&digest)
        .execute(&mut *tx)
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_revoke"))?;
    }
    tx.commit()
        .await
        .map_err(|error| database_unavailable(&error, "mcp_oauth_revoke_commit"))?;
    Ok(no_store(StatusCode::OK.into_response()))
}

// `Response` is the frozen HTTP authentication boundary; boxing it would change callers' API.
#[allow(clippy::result_large_err)]
pub(super) async fn authenticate_mcp(
    state: &ApiState,
    headers: &HeaderMap,
) -> Result<McpPrincipal, Response> {
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .ok_or_else(|| bearer_error(state, StatusCode::UNAUTHORIZED, "invalid_token"))?;
    let row =
        sqlx::query("SELECT vault_id,device_id,grant_id,scopes,token_digest,family_id,resource,device_role FROM mcp_resolve_access_token($1)")
            .bind(digest(bearer))
            .fetch_optional(&state.db)
            .await
            .map_err(|error| bearer_database_error(state, &error, "mcp_oauth_access_resolve"))?
            .ok_or_else(|| bearer_error(state, StatusCode::UNAUTHORIZED, "invalid_token"))?;
    let principal = McpPrincipal {
        vault_id: row.get("vault_id"),
        device_id: row.get("device_id"),
        grant_id: row.get("grant_id"),
        scopes: scopes_from_strings(row.get("scopes"))
            .map_err(|_| bearer_error(state, StatusCode::UNAUTHORIZED, "invalid_token"))?,
        token_digest: row.get("token_digest"),
        family_id: row.get("family_id"),
        resource: row.get("resource"),
        device_role: row.get("device_role"),
    };
    if principal.resource != resource(state).map_err(|error| error.into_response())?
        || !state
            .policy
            .authorize(principal.access_principal(), Operation::Read)
            .await
    {
        return Err(bearer_error(
            state,
            StatusCode::FORBIDDEN,
            "insufficient_scope",
        ));
    }
    Ok(principal)
}

// `Response` is the frozen HTTP authentication boundary; boxing it would change callers' API.
#[allow(clippy::result_large_err)]
pub(super) async fn revalidate_mcp(
    state: &ApiState,
    principal: &McpPrincipal,
) -> Result<(), Response> {
    let mut tx = scope::begin(&state.db, principal.vault_id)
        .await
        .map_err(|error| bearer_database_error(state, &error, "mcp_oauth_revalidate_begin"))?;
    let expected_resource = resource(state).map_err(|error| error.into_response())?;
    let live: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mcp_oauth_tokens t JOIN mcp_grants g ON g.id=t.grant_id AND g.vault_id=t.vault_id AND g.device_id=t.device_id JOIN mcp_devices m ON m.vault_id=t.vault_id AND m.device_id=t.device_id JOIN devices d ON d.vault_id=t.vault_id AND d.device_id=t.device_id WHERE t.digest=$1 AND t.family_id=$2 AND t.token_kind='access' AND t.expires_at>clock_timestamp() AND t.revoked_at IS NULL AND t.resource=$3 AND g.id=$4 AND g.vault_id=$5 AND g.device_id=$6 AND g.revoked_at IS NULL AND d.revoked_at IS NULL)").bind(&principal.token_digest).bind(principal.family_id).bind(expected_resource).bind(principal.grant_id).bind(principal.vault_id).bind(principal.device_id).fetch_one(&mut *tx).await.map_err(|error| bearer_database_error(state, &error, "mcp_oauth_revalidate"))?;
    if !live {
        return Err(bearer_error(
            state,
            StatusCode::UNAUTHORIZED,
            "invalid_token",
        ));
    }
    tx.commit()
        .await
        .map_err(|error| bearer_database_error(state, &error, "mcp_oauth_revalidate_commit"))?;
    Ok(())
}

fn validate_redirect(value: &str) -> ApiResult<url::Url> {
    let url = url::Url::parse(value)
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid_redirect_uri"))?;
    let loopback = url.scheme() == "http" && is_loopback_redirect(&url);
    if url.fragment().is_some()
        || url.username() != ""
        || url.password().is_some()
        || !(url.scheme() == "https" || loopback)
    {
        return Err(ApiError(StatusCode::BAD_REQUEST, "invalid_redirect_uri"));
    }
    Ok(url)
}
fn is_loopback_redirect(url: &url::Url) -> bool {
    matches!(
        url.host_str(),
        Some("127.0.0.1") | Some("[::1]") | Some("localhost")
    )
}
fn required(value: &Option<String>) -> ApiResult<&str> {
    value
        .as_deref()
        .filter(|value| !value.is_empty())
        .ok_or(ApiError(StatusCode::BAD_REQUEST, "invalid_request"))
}
async fn require_client(state: &ApiState, client_id: &str) -> ApiResult<()> {
    let known =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM mcp_oauth_clients WHERE client_id=$1)")
            .bind(client_id)
            .fetch_one(&state.db)
            .await
            .map_err(|error| database_unavailable(&error, "mcp_oauth_client_lookup"))?;
    if known {
        Ok(())
    } else {
        Err(ApiError(StatusCode::BAD_REQUEST, "invalid_client"))
    }
}
fn parse_scopes(value: &str) -> ApiResult<Vec<McpScope>> {
    let scopes = value
        .split_ascii_whitespace()
        .map(|scope| match scope {
            "conversations:read" => Ok(McpScope::ConversationsRead),
            "messages:send" => Ok(McpScope::MessagesSend),
            "media:send" => Ok(McpScope::MediaSend),
            _ => Err(ApiError(StatusCode::BAD_REQUEST, "invalid_scope")),
        })
        .collect::<ApiResult<Vec<_>>>()?;
    if !McpScope::is_valid_set(&scopes) {
        Err(ApiError(StatusCode::BAD_REQUEST, "invalid_scope"))
    } else {
        Ok(scopes)
    }
}
fn scopes_from_strings(scopes: Vec<String>) -> ApiResult<Vec<McpScope>> {
    parse_scopes(&scopes.join(" "))
}
fn scope_strings(scopes: &[McpScope]) -> Vec<String> {
    scopes.iter().map(ToString::to_string).collect()
}
fn scope_names() -> [&'static str; 3] {
    ["conversations:read", "messages:send", "media:send"]
}
fn valid_pkce(value: &str) -> bool {
    value.len() == 43
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}
fn opaque_secret() -> String {
    let first = Uuid::new_v4();
    let second = Uuid::new_v4();
    let mut bytes = Vec::with_capacity(32);
    bytes.extend_from_slice(first.as_bytes());
    bytes.extend_from_slice(second.as_bytes());
    URL_SAFE_NO_PAD.encode(bytes)
}
fn digest(value: &str) -> Vec<u8> {
    Sha256::digest(value.as_bytes()).to_vec()
}
fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}
fn oauth_error(error: ApiError) -> Response {
    let code = if error.0 == StatusCode::SERVICE_UNAVAILABLE && error.1 == "database_unavailable" {
        "temporarily_unavailable"
    } else {
        error.1
    };
    no_store((error.0, Json(serde_json::json!({"error":code}))).into_response())
}
fn bearer_error(state: &ApiState, status: StatusCode, error: &str) -> Response {
    let mut response = (status, Json(serde_json::json!({"error":error}))).into_response();
    if status == StatusCode::UNAUTHORIZED
        && let Ok(metadata) = origin(state).and_then(|origin| {
            origin
                .join(".well-known/oauth-protected-resource")
                .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "mcp_origin_unavailable"))
        })
        && let Ok(challenge) =
            HeaderValue::from_str(&format!("Bearer resource_metadata=\"{metadata}\""))
    {
        response
            .headers_mut()
            .insert(header::WWW_AUTHENTICATE, challenge);
    }
    no_store(response)
}

fn bearer_database_error(
    state: &ApiState,
    error: &sqlx::Error,
    operation: &'static str,
) -> Response {
    let unavailable = database_unavailable(error, operation);
    bearer_error(state, unavailable.0, "temporarily_unavailable")
}
