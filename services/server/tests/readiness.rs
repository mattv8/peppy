use std::{net::SocketAddr, time::Duration};

use peppy_server::{
    config::{Config, S3Config},
    health::storage_contract_check,
    storage::Storage,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_VALUE: &str = "AES256";
const REQUESTED_KEY: &str = "__REQUESTED_KEY__";
const LIST_QUERY: &str = "versions=&prefix=__REQUESTED_KEY__&max-keys=1000";

#[tokio::test]
async fn put_bytes_signs_and_requires_the_sse_acknowledgement() {
    let (config, server) = mock_server(vec![ExpectedRequest::put(
        "/private/object",
        Response::ok().with_header(SSE_HEADER, SSE_VALUE),
    )])
    .await;

    Storage::new(&config)
        .put_bytes("object", b"private bytes".to_vec())
        .await
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn put_bytes_rejects_a_missing_sse_acknowledgement() {
    let (config, server) = mock_server(vec![ExpectedRequest::put(
        "/private/object",
        Response::ok(),
    )])
    .await;

    let error = Storage::new(&config)
        .put_bytes("object", b"private bytes".to_vec())
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 PUT encryption acknowledgement missing"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn put_bytes_rejects_a_wrong_sse_acknowledgement() {
    let (config, server) = mock_server(vec![ExpectedRequest::put(
        "/private/object",
        Response::ok().with_header(SSE_HEADER, "aws:kms"),
    )])
    .await;

    let error = Storage::new(&config)
        .put_bytes("object", b"private bytes".to_vec())
        .await
        .unwrap_err();

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 PUT encryption acknowledgement missing"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn put_file_signs_and_requires_the_sse_acknowledgement() {
    let (config, server) = mock_server(vec![ExpectedRequest::put(
        "/private/object",
        Response::ok().with_header(SSE_HEADER, SSE_VALUE),
    )])
    .await;
    let file = tempfile::NamedTempFile::new().unwrap();
    std::fs::write(file.path(), b"private file bytes").unwrap();

    Storage::new(&config)
        .put_file("object", file.path().to_path_buf())
        .await
        .unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_probes_precreated_buckets_and_removes_its_probe_object() {
    let (config, server) = mock_server(probe_requests(false, ProbeResponse::Valid)).await;

    let storage = Storage::initialize(&config).await.unwrap();
    assert!(!storage.is_versioned());
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_treats_a_null_version_id_as_unversioned() {
    let mut requests = probe_requests(false, ProbeResponse::Valid);
    requests[0].response = Response::ok()
        .with_header(SSE_HEADER, SSE_VALUE)
        .with_header("x-amz-version-id", "null");
    let (config, server) = mock_server(requests).await;

    let storage = Storage::initialize(&config).await.unwrap();

    assert!(!storage.is_versioned());
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_detects_versioning_and_removes_the_recorded_version() {
    let (config, server) = mock_server(versioned_probe_requests(
        Response::ok()
            .with_header(SSE_HEADER, SSE_VALUE)
            .with_header("x-amz-version-id", "V1"),
        Response::new("204 No Content"),
        empty_list_response(),
    ))
    .await;

    let storage = Storage::initialize(&config).await.unwrap();

    assert!(storage.is_versioned());
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_cleans_up_a_versioned_put_when_its_sse_acknowledgement_is_missing() {
    let (config, server) = mock_server(versioned_probe_requests(
        Response::ok().with_header("x-amz-version-id", "V1"),
        Response::new("204 No Content"),
        empty_list_response(),
    ))
    .await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 PUT encryption acknowledgement missing"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_purges_only_leftover_versions_for_the_probe_key() {
    let mut requests = versioned_probe_requests(
        Response::ok()
            .with_header(SSE_HEADER, SSE_VALUE)
            .with_header("x-amz-version-id", "V1"),
        Response::new("204 No Content"),
        Response::ok().with_body(list_versions_body(
            &[
                (REQUESTED_KEY, "V2", true),
                ("__REQUESTED_KEY__-more", "V3", false),
            ],
            None,
        )),
    );
    requests.push(
        ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-storage-probe-",
            Response::new("204 No Content"),
        )
        .with_required_query("versionId=V2"),
    );
    requests.push(
        ExpectedRequest::new(
            "GET",
            "/private?versions=&prefix=peppy-storage-probe-",
            empty_list_response(),
        )
        .with_required_query(LIST_QUERY),
    );
    let (config, server) = mock_server(requests).await;

    Storage::initialize(&config).await.unwrap();

    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_a_nonempty_relist_after_versioned_purge() {
    let mut requests = versioned_probe_requests(
        Response::ok()
            .with_header(SSE_HEADER, SSE_VALUE)
            .with_header("x-amz-version-id", "V1"),
        Response::new("204 No Content"),
        Response::ok().with_body(list_versions_body(&[(REQUESTED_KEY, "V2", false)], None)),
    );
    requests.push(
        ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-storage-probe-",
            Response::new("204 No Content"),
        )
        .with_required_query("versionId=V2"),
    );
    requests.push(
        ExpectedRequest::new_prefix(
            "GET",
            "/private?versions=&prefix=peppy-storage-probe-",
            Response::ok().with_body(list_versions_body(&[(REQUESTED_KEY, "V2", false)], None)),
        )
        .with_required_query(LIST_QUERY),
    );
    let (config, server) = mock_server(requests).await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 purge incomplete"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_forbidden_version_deletes() {
    let (config, server) = mock_server(without_cleanup_listing(versioned_probe_requests(
        versioned_put_response(),
        Response::new("403 Forbidden"),
        empty_list_response(),
    )))
    .await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 version delete forbidden"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_forbidden_version_listings() {
    let (config, server) = mock_server(versioned_probe_requests(
        versioned_put_response(),
        Response::new("204 No Content"),
        Response::new("403 Forbidden"),
    ))
    .await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 version listing forbidden"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_accepts_a_missing_recorded_version() {
    let (config, server) = mock_server(versioned_probe_requests(
        versioned_put_response(),
        Response::new("404 Not Found"),
        empty_list_response(),
    ))
    .await;

    Storage::initialize(&config).await.unwrap();

    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_version_delete_failures() {
    let (config, server) = mock_server(without_cleanup_listing(versioned_probe_requests(
        versioned_put_response(),
        Response::new("500 Internal Server Error"),
        empty_list_response(),
    )))
    .await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 version delete rejected"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_malformed_version_listings() {
    let (config, server) = mock_server(versioned_probe_requests(
        versioned_put_response(),
        Response::new("204 No Content"),
        Response::ok().with_body(b"not xml".to_vec()),
    ))
    .await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 version listing malformed"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_paginates_version_listings_and_deletes_each_version() {
    let mut requests = versioned_probe_requests(
        versioned_put_response(),
        Response::new("204 No Content"),
        Response::ok().with_body(list_versions_body(
            &[(REQUESTED_KEY, "a/b+c=", false)],
            Some((REQUESTED_KEY, "a/b+c=")),
        )),
    );
    requests.push(
        ExpectedRequest::new(
            "GET",
            "/private?versions=&prefix=peppy-storage-probe-",
            Response::ok().with_body(list_versions_body(
                &[(REQUESTED_KEY, "v/2+=", true)],
                None,
            )),
        )
        .with_required_query(
            "versions=&prefix=__REQUESTED_KEY__&max-keys=1000&key-marker=__REQUESTED_KEY__&version-id-marker=a%2Fb%2Bc%3D",
        ),
    );
    requests.push(
        ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-storage-probe-",
            Response::new("204 No Content"),
        )
        .with_required_query("versionId=a%2Fb%2Bc%3D"),
    );
    requests.push(
        ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-storage-probe-",
            Response::new("204 No Content"),
        )
        .with_required_query("versionId=v%2F2%2B%3D"),
    );
    requests.push(
        ExpectedRequest::new(
            "GET",
            "/private?versions=&prefix=peppy-storage-probe-",
            empty_list_response(),
        )
        .with_required_query(LIST_QUERY),
    );
    let (config, server) = mock_server(requests).await;

    Storage::initialize(&config).await.unwrap();

    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_version_listings_larger_than_one_mebibyte() {
    let (config, server) = mock_server(versioned_probe_requests(
        versioned_put_response(),
        Response::new("204 No Content"),
        Response::ok().with_body(vec![b'x'; 1_048_577]),
    ))
    .await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 version listing too large"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_deletes_a_null_version_list_entry() {
    let mut requests = versioned_probe_requests(
        versioned_put_response(),
        Response::new("204 No Content"),
        Response::ok().with_body(list_versions_body(&[(REQUESTED_KEY, "null", false)], None)),
    );
    requests.push(
        ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-storage-probe-",
            Response::new("204 No Content"),
        )
        .with_required_query("versionId=null"),
    );
    requests.push(
        ExpectedRequest::new(
            "GET",
            "/private?versions=&prefix=peppy-storage-probe-",
            empty_list_response(),
        )
        .with_required_query(LIST_QUERY),
    );
    let (config, server) = mock_server(requests).await;

    Storage::initialize(&config).await.unwrap();

    server.await.unwrap();
}

#[tokio::test]
async fn purge_on_new_storage_uses_a_plain_delete() {
    let (config, server) = mock_server(vec![ExpectedRequest::new(
        "DELETE",
        "/private/object",
        Response::new("204 No Content"),
    )])
    .await;

    Storage::new(&config).purge("object").await.unwrap();

    server.await.unwrap();
}

#[tokio::test]
async fn storage_contract_check_cleans_up_a_versioned_object_after_a_bad_signed_read() {
    let mut requests = versioned_probe_requests(
        versioned_put_response(),
        Response::new("204 No Content"),
        empty_list_response(),
    );
    requests.push(ExpectedRequest::put_prefix(
        "/private/peppy-readiness-check-",
        Response::ok()
            .with_header(SSE_HEADER, SSE_VALUE)
            .with_header("x-amz-version-id", "R1"),
    ));
    requests.push(ExpectedRequest::new_prefix(
        "GET",
        "/private/peppy-readiness-check-",
        Response::ok()
            .with_header(SSE_HEADER, SSE_VALUE)
            .with_body(b"wrong body".to_vec()),
    ));
    requests.push(
        ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-readiness-check-",
            Response::new("204 No Content"),
        )
        .with_required_query("versionId=R1"),
    );
    requests.push(
        ExpectedRequest::new(
            "GET",
            "/private?versions=&prefix=peppy-readiness-check-",
            empty_list_response(),
        )
        .with_required_query(LIST_QUERY),
    );
    let (s3, server) = mock_server(requests).await;
    let config = test_config(s3);

    let error = storage_contract_check(&config).await.unwrap_err();

    assert_eq!(
        error.to_string(),
        "signed S3 round trip returned an unexpected body"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_provisions_then_probes_a_new_bucket() {
    let (mut config, server) = mock_server(probe_requests(true, ProbeResponse::Valid)).await;
    config.precreated_bucket = false;

    Storage::initialize(&config).await.unwrap();
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_an_incorrect_head_sse_acknowledgement() {
    let (config, server) = mock_server(probe_requests(false, ProbeResponse::WrongHeadAck)).await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 HEAD encryption acknowledgement missing"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_a_missing_head_sse_acknowledgement() {
    let (config, server) = mock_server(probe_requests(false, ProbeResponse::MissingHeadAck)).await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 HEAD encryption acknowledgement missing"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_a_missing_full_get_sse_acknowledgement() {
    let (config, server) =
        mock_server(probe_requests(false, ProbeResponse::MissingFullGetAck)).await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 GET encryption acknowledgement missing"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_a_missing_ranged_get_sse_acknowledgement() {
    let (config, server) =
        mock_server(probe_requests(false, ProbeResponse::MissingRangeGetAck)).await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 GET encryption acknowledgement missing"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_corrupt_full_probe_data_and_still_deletes_it() {
    let (config, server) = mock_server(probe_requests(false, ProbeResponse::CorruptFull)).await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 encryption probe full read mismatch"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_rejects_corrupt_ranged_probe_data_and_still_deletes_it() {
    let (config, server) = mock_server(probe_requests(false, ProbeResponse::CorruptRange)).await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(
        error.to_string(),
        "storage unavailable: S3 encryption probe range read mismatch"
    );
    server.await.unwrap();
}

#[tokio::test]
async fn initialization_fails_when_probe_cleanup_is_rejected() {
    let (config, server) = mock_server(probe_requests(false, ProbeResponse::CleanupFailure)).await;

    let error = initialization_error(Storage::initialize(&config).await);

    assert_eq!(error.to_string(), "storage unavailable: S3 DELETE rejected");
    server.await.unwrap();
}

#[derive(Clone, Copy)]
enum ProbeResponse {
    Valid,
    MissingHeadAck,
    MissingFullGetAck,
    MissingRangeGetAck,
    WrongHeadAck,
    CorruptFull,
    CorruptRange,
    CleanupFailure,
}

fn initialization_error(
    result: Result<Storage, peppy_server::storage::StorageError>,
) -> peppy_server::storage::StorageError {
    match result {
        Ok(_) => panic!("storage initialization unexpectedly succeeded"),
        Err(error) => error,
    }
}

fn probe_requests(provision_bucket: bool, response: ProbeResponse) -> Vec<ExpectedRequest> {
    let mut requests = Vec::new();
    if provision_bucket {
        requests.push(ExpectedRequest::new("PUT", "/private", Response::ok()));
    }
    requests.push(ExpectedRequest::put_prefix(
        "/private/peppy-storage-probe-",
        Response::ok().with_header(SSE_HEADER, SSE_VALUE),
    ));
    let head_ack = match response {
        ProbeResponse::WrongHeadAck => "aws:kms",
        _ => SSE_VALUE,
    };
    let mut head_response = Response::ok().with_header("content-length", "20971520");
    if !matches!(response, ProbeResponse::MissingHeadAck) {
        head_response = head_response.with_header(SSE_HEADER, head_ack);
    }
    requests.push(ExpectedRequest::new_prefix(
        "HEAD",
        "/private/peppy-storage-probe-",
        head_response,
    ));
    if matches!(
        response,
        ProbeResponse::MissingHeadAck | ProbeResponse::WrongHeadAck
    ) {
        requests.push(ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-storage-probe-",
            Response::ok(),
        ));
        return requests;
    }
    let full_body = match response {
        ProbeResponse::CorruptFull => probe_bytes_with_change(0),
        _ => probe_bytes(),
    };
    let mut full_response = Response::ok().with_body(full_body);
    if !matches!(response, ProbeResponse::MissingFullGetAck) {
        full_response = full_response.with_header(SSE_HEADER, SSE_VALUE);
    }
    requests.push(ExpectedRequest::new_prefix(
        "GET",
        "/private/peppy-storage-probe-",
        full_response,
    ));
    if matches!(response, ProbeResponse::MissingFullGetAck) {
        requests.push(ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-storage-probe-",
            Response::ok(),
        ));
        return requests;
    }
    if matches!(response, ProbeResponse::CorruptFull) {
        requests.push(ExpectedRequest::new_prefix(
            "DELETE",
            "/private/peppy-storage-probe-",
            Response::ok(),
        ));
        return requests;
    }
    let range_body = match response {
        ProbeResponse::CorruptRange => vec![0; 64],
        _ => probe_range_bytes(),
    };
    let mut range_response = Response::partial(range_body);
    if !matches!(response, ProbeResponse::MissingRangeGetAck) {
        range_response = range_response.with_header(SSE_HEADER, SSE_VALUE);
    }
    requests.push(
        ExpectedRequest::new_prefix("GET", "/private/peppy-storage-probe-", range_response)
            .with_required_header("range", "bytes=8388621-8388684"),
    );
    let cleanup = if matches!(response, ProbeResponse::CleanupFailure) {
        Response::new("500 Internal Server Error")
    } else {
        Response::ok()
    };
    requests.push(ExpectedRequest::new_prefix(
        "DELETE",
        "/private/peppy-storage-probe-",
        cleanup,
    ));
    requests
}

fn versioned_put_response() -> Response {
    Response::ok()
        .with_header(SSE_HEADER, SSE_VALUE)
        .with_header("x-amz-version-id", "V1")
}

fn versioned_probe_requests(
    put_response: Response,
    delete_response: Response,
    list_response: Response,
) -> Vec<ExpectedRequest> {
    let mut requests = vec![ExpectedRequest::put_prefix(
        "/private/peppy-storage-probe-",
        put_response,
    )];
    if !requests[0]
        .response
        .headers
        .contains(&(SSE_HEADER, SSE_VALUE))
    {
        requests.push(
            ExpectedRequest::new_prefix("DELETE", "/private/peppy-storage-probe-", delete_response)
                .with_required_query("versionId=V1"),
        );
        requests.push(
            ExpectedRequest::new(
                "GET",
                "/private?versions=&prefix=peppy-storage-probe-",
                list_response,
            )
            .with_required_query(LIST_QUERY),
        );
        return requests;
    }
    requests.push(ExpectedRequest::new_prefix(
        "HEAD",
        "/private/peppy-storage-probe-",
        Response::ok()
            .with_header("content-length", "20971520")
            .with_header(SSE_HEADER, SSE_VALUE),
    ));
    requests.push(ExpectedRequest::new_prefix(
        "GET",
        "/private/peppy-storage-probe-",
        Response::ok()
            .with_header(SSE_HEADER, SSE_VALUE)
            .with_body(probe_bytes()),
    ));
    requests.push(
        ExpectedRequest::new_prefix(
            "GET",
            "/private/peppy-storage-probe-",
            Response::partial(probe_range_bytes()).with_header(SSE_HEADER, SSE_VALUE),
        )
        .with_required_header("range", "bytes=8388621-8388684"),
    );
    requests.push(
        ExpectedRequest::new_prefix("DELETE", "/private/peppy-storage-probe-", delete_response)
            .with_required_query("versionId=V1"),
    );
    requests.push(
        ExpectedRequest::new(
            "GET",
            "/private?versions=&prefix=peppy-storage-probe-",
            list_response,
        )
        .with_required_query(LIST_QUERY),
    );
    requests
}

/// A failed exact-version delete ends cleanup, so no follow-up listing is sent.
fn without_cleanup_listing(mut requests: Vec<ExpectedRequest>) -> Vec<ExpectedRequest> {
    requests.pop();
    requests
}

fn empty_list_response() -> Response {
    Response::ok().with_body(list_versions_body(&[], None))
}

fn list_versions_body(entries: &[(&str, &str, bool)], markers: Option<(&str, &str)>) -> Vec<u8> {
    let mut body = String::from(
        r#"<?xml version="1.0" encoding="UTF-8"?><ListVersionsResult xmlns="http://s3.amazonaws.com/doc/2006-03-01/"><IsTruncated>"#,
    );
    body.push_str(if markers.is_some() { "true" } else { "false" });
    body.push_str("</IsTruncated>");
    if let Some((key_marker, version_marker)) = markers {
        body.push_str(&format!(
            "<NextKeyMarker>{key_marker}</NextKeyMarker><NextVersionIdMarker>{version_marker}</NextVersionIdMarker>"
        ));
    }
    for (key, version, delete_marker) in entries {
        let element = if *delete_marker {
            "DeleteMarker"
        } else {
            "Version"
        };
        body.push_str(&format!(
            "<{element}><Key>{key}</Key><VersionId>{version}</VersionId></{element}>"
        ));
    }
    body.push_str("</ListVersionsResult>");
    body.into_bytes()
}

fn test_config(s3: S3Config) -> Config {
    Config {
        bind_addr: "127.0.0.1:0".parse::<SocketAddr>().unwrap(),
        database_url: "postgres://ignored:ignored@127.0.0.1:1/ignored".into(),
        release_identity: "readiness-test".into(),
        s3: Some(s3),
        public_api_url: None,
        public_attachment_url: None,
        vault_attachment_quota_bytes: 512 * 1024 * 1024,
        trusted_proxy_cidrs: Vec::new(),
        replay_retention: Duration::from_secs(86_400),
        relay_url: None,
        web_client: None,
    }
}

fn probe_bytes() -> Vec<u8> {
    (0..20 * 1024 * 1024)
        .map(|position| (position % 251) as u8)
        .collect()
}

fn probe_bytes_with_change(position: usize) -> Vec<u8> {
    let mut bytes = probe_bytes();
    bytes[position] ^= 0xff;
    bytes
}

fn probe_range_bytes() -> Vec<u8> {
    (8_388_621..=8_388_684)
        .map(|position| (position % 251) as u8)
        .collect()
}

struct ExpectedRequest {
    method: &'static str,
    path_prefix: &'static str,
    required_header: Option<(&'static str, &'static str)>,
    required_query: Option<&'static str>,
    response: Response,
}

impl ExpectedRequest {
    fn new(method: &'static str, path: &'static str, response: Response) -> Self {
        Self {
            method,
            path_prefix: path,
            required_header: None,
            required_query: None,
            response,
        }
    }

    fn new_prefix(method: &'static str, path_prefix: &'static str, response: Response) -> Self {
        Self {
            method,
            path_prefix,
            required_header: None,
            required_query: None,
            response,
        }
    }

    fn put(path: &'static str, response: Response) -> Self {
        Self::new("PUT", path, response)
    }

    fn put_prefix(path_prefix: &'static str, response: Response) -> Self {
        Self::new_prefix("PUT", path_prefix, response)
    }

    fn with_required_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.required_header = Some((name, value));
        self
    }

    fn with_required_query(mut self, query: &'static str) -> Self {
        self.required_query = Some(query);
        self
    }
}

struct Response {
    status: &'static str,
    headers: Vec<(&'static str, &'static str)>,
    body: Vec<u8>,
}

impl Response {
    fn new(status: &'static str) -> Self {
        Self {
            status,
            headers: vec![],
            body: vec![],
        }
    }
    fn ok() -> Self {
        Self {
            status: "200 OK",
            headers: vec![],
            body: vec![],
        }
    }
    fn partial(body: Vec<u8>) -> Self {
        Self {
            status: "206 Partial Content",
            headers: vec![
                ("content-length", "64"),
                ("content-range", "bytes 8388621-8388684/20971520"),
            ],
            body,
        }
    }
    fn with_header(mut self, name: &'static str, value: &'static str) -> Self {
        self.headers.push((name, value));
        self
    }
    fn with_body(mut self, body: Vec<u8>) -> Self {
        self.body = body;
        self
    }
}

async fn mock_server(expected: Vec<ExpectedRequest>) -> (S3Config, tokio::task::JoinHandle<()>) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        for expected in expected {
            let (mut stream, _) = listener.accept().await.unwrap();
            let request = read_request(&mut stream).await;
            let headers_end = request
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .unwrap()
                + 4;
            let headers = std::str::from_utf8(&request[..headers_end]).unwrap();
            let request_line = headers.lines().next().unwrap();
            let target = request_line.split_whitespace().nth(1).unwrap();
            assert!(
                request_line.starts_with(&format!("{} {}", expected.method, expected.path_prefix)),
                "unexpected request: {request_line}"
            );
            if let Some(query) = expected.required_query {
                let query = replace_requested_key(query.as_bytes(), target);
                let query = std::str::from_utf8(&query).unwrap();
                assert!(
                    target.contains(query),
                    "unexpected request target: {target}"
                );
            } else {
                assert!(!target.contains('?'), "unexpected request target: {target}");
            }
            assert!(headers.contains("\r\nauthorization: AWS4-HMAC-SHA256 "));
            if expected.method == "PUT" && expected.path_prefix != "/private" {
                assert!(headers.contains("\r\nx-amz-server-side-encryption: AES256\r\n"));
                let signed_headers = headers
                    .lines()
                    .find_map(|line| line.strip_prefix("authorization: "))
                    .and_then(|authorization| authorization.split("SignedHeaders=").nth(1))
                    .and_then(|value| value.split(',').next())
                    .expect("authorization must contain SignedHeaders");
                assert!(signed_headers.split(';').any(|name| name == SSE_HEADER));
            }
            if let Some((name, value)) = expected.required_header {
                assert!(headers.contains(&format!("\r\n{name}: {value}\r\n")));
            }
            let body = replace_requested_key(&expected.response.body, target);
            let mut response = format!(
                "HTTP/1.1 {}\r\nConnection: close\r\n",
                expected.response.status
            );
            for (name, value) in expected.response.headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
            stream.write_all(response.as_bytes()).await.unwrap();
            let _ = stream.write_all(&body).await;
        }
    });
    (
        S3Config {
            endpoint: Url::parse(&format!("http://{address}")).unwrap(),
            bucket: "private".into(),
            access_key: "testaccess".into(),
            secret_key: "testsecret".into(),
            signing_region: "us-east-1".into(),
            precreated_bucket: true,
            readiness_timeout: Duration::from_secs(2),
        },
        server,
    )
}

fn replace_requested_key(body: &[u8], target: &str) -> Vec<u8> {
    let prefix = requested_key(target);
    match prefix {
        Some(prefix)
            if body
                .windows(REQUESTED_KEY.len())
                .any(|window| window == REQUESTED_KEY.as_bytes()) =>
        {
            String::from_utf8(body.to_vec())
                .unwrap()
                .replace(REQUESTED_KEY, &prefix)
                .into_bytes()
        }
        _ => body.to_vec(),
    }
}

fn requested_key(target: &str) -> Option<String> {
    Url::parse(&format!("http://mock{target}"))
        .unwrap()
        .query_pairs()
        .find_map(|(name, value)| (name == "prefix").then(|| value.into_owned()))
}

async fn read_request(stream: &mut tokio::net::TcpStream) -> Vec<u8> {
    let mut request = Vec::new();
    loop {
        let mut chunk = [0_u8; 8192];
        let read = stream.read(&mut chunk).await.unwrap();
        assert!(read > 0, "request ended before its headers");
        request.extend_from_slice(&chunk[..read]);
        if let Some(headers_end) = request.windows(4).position(|window| window == b"\r\n\r\n") {
            let headers_end = headers_end + 4;
            let headers = std::str::from_utf8(&request[..headers_end]).unwrap();
            let body_length = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length: "))
                .and_then(|value| value.parse::<usize>().ok())
                .unwrap_or(0);
            while request.len() < headers_end + body_length {
                let read = stream.read(&mut chunk).await.unwrap();
                assert!(read > 0, "request ended before its body");
                request.extend_from_slice(&chunk[..read]);
            }
            return request;
        }
    }
}
