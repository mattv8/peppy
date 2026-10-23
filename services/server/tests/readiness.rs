use std::time::Duration;

use peppy_server::{config::S3Config, storage::Storage};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::Url;

const SSE_HEADER: &str = "x-amz-server-side-encryption";
const SSE_VALUE: &str = "AES256";

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

    Storage::initialize(&config).await.unwrap();
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
    response: Response,
}

impl ExpectedRequest {
    fn new(method: &'static str, path: &'static str, response: Response) -> Self {
        Self {
            method,
            path_prefix: path,
            required_header: None,
            response,
        }
    }

    fn new_prefix(method: &'static str, path_prefix: &'static str, response: Response) -> Self {
        Self {
            method,
            path_prefix,
            required_header: None,
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
            assert!(
                request_line.starts_with(&format!("{} {}", expected.method, expected.path_prefix)),
                "unexpected request: {request_line}"
            );
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
            let mut response = format!(
                "HTTP/1.1 {}\r\nConnection: close\r\n",
                expected.response.status
            );
            for (name, value) in expected.response.headers {
                response.push_str(&format!("{name}: {value}\r\n"));
            }
            response.push_str(&format!(
                "Content-Length: {}\r\n\r\n",
                expected.response.body.len()
            ));
            stream.write_all(response.as_bytes()).await.unwrap();
            let _ = stream.write_all(&expected.response.body).await;
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
