use std::time::{Duration, SystemTime};

use aws_credential_types::Credentials;
use aws_sigv4::{
    http_request::{
        PayloadChecksumKind, PercentEncodingMode, SignableBody, SignableRequest, SigningParams,
        SigningSettings, UriPathNormalizationMode, sign,
    },
    sign::v4,
};
use peppy_server::{
    config::{Config, S3Config},
    storage::{Storage, StorageError, StorageUnavailableReason},
};
use reqwest::{
    Client, Method, Response,
    header::{HeaderName, HeaderValue},
};
use url::Url;
use uuid::Uuid;

#[tokio::test]
async fn truncated_version_listing_root_is_an_empty_page() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = axum::Router::new().fallback(|| async { "<ListVersionsResult>" });
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let config = S3Config {
        endpoint: Url::parse(&format!("http://{address}")).unwrap(),
        bucket: "test-bucket".into(),
        access_key: "test-access".into(),
        secret_key: "test-secret".into(),
        signing_region: "test-region".into(),
        precreated_bucket: true,
        readiness_timeout: Duration::from_secs(1),
    };
    // Pins current behaviour of review finding D-18; flip this assertion when the fix is authorized.
    assert!(
        Storage::new(&config)
            .list_versions("key")
            .await
            .unwrap()
            .is_empty()
    );
    server.abort();
}

#[tokio::test]
async fn seaweedfs_fixture_supports_versioned_purge() {
    let config = Config::from_env().expect("complete S3 test configuration is required");
    let default_s3 = config
        .s3
        .as_ref()
        .expect("S3 is required for storage version tests");
    let default_storage = Storage::initialize(default_s3).await.unwrap();
    let default_key = format!(
        "private/test/default-versioning-{}",
        Uuid::new_v4().simple()
    );
    let default_versions = default_storage.list_versions(&default_key).await;
    // SeaweedFS 4.48 has returned a non-ListVersionsResult reply for `?versions` on
    // an unversioned bucket. That malformed-listing result must not be interpreted as no versions.
    assert!(!default_storage.is_versioned());
    match default_versions {
        Ok(versions) => assert!(versions.is_empty()),
        Err(error) => assert_eq!(
            error,
            StorageError::Unavailable(StorageUnavailableReason::VersionListingMalformed)
        ),
    }

    let bucket = format!("peppy-versioned-{}", Uuid::new_v4().simple());
    let bucket_url = bucket_url(&default_s3.endpoint, &bucket);
    let create = signed_request(default_s3, Method::PUT, &bucket_url, "", &[])
        .await
        .unwrap();
    assert!(
        create.status().is_success(),
        "CreateBucket rejected: {} {}",
        create.status(),
        create.text().await.unwrap_or_default(),
    );

    let versioning_url = Url::parse(&format!("{bucket_url}?versioning=")).unwrap();
    let versioning_body = "<VersioningConfiguration xmlns=\"http://s3.amazonaws.com/doc/2006-03-01/\"><Status>Enabled</Status></VersioningConfiguration>";
    let enable = signed_request(
        default_s3,
        Method::PUT,
        &versioning_url,
        versioning_body,
        &[("content-type", "application/xml")],
    )
    .await
    .unwrap();
    if !enable.status().is_success() {
        let response = format!(
            "{} {}",
            enable.status(),
            enable.text().await.unwrap_or_default()
        );
        delete_bucket(default_s3, &bucket_url).await;
        panic!("PutBucketVersioning rejected: {response}");
    }

    let versioned_s3 = S3Config {
        bucket: bucket.clone(),
        precreated_bucket: true,
        ..default_s3.clone()
    };
    let storage = match Storage::initialize(&versioned_s3).await {
        Ok(storage) => storage,
        Err(error) => {
            delete_bucket(default_s3, &bucket_url).await;
            panic!("versioned bucket initialization failed: {error}");
        }
    };
    let key = format!("private/test/{}", Uuid::new_v4().simple());
    let test_result: Result<(), String> = async {
        if !storage.is_versioned() {
            return Err("versioned bucket initialization did not report versioning".into());
        }
        if !storage
            .list_versions(&key)
            .await
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            return Err("the fresh versioned bucket probe key was not empty".into());
        }
        storage
            .put_bytes(&key, b"first version".to_vec())
            .await
            .map_err(|error| error.to_string())?;
        storage
            .put_bytes(&key, b"second version".to_vec())
            .await
            .map_err(|error| error.to_string())?;
        storage
            .delete(&key)
            .await
            .map_err(|error| error.to_string())?;

        let versions = storage
            .list_versions(&key)
            .await
            .map_err(|error| error.to_string())?;
        if versions.len() != 3 {
            return Err(format!(
                "expected 3 version entries, got {}",
                versions.len()
            ));
        }
        if versions.iter().filter(|entry| entry.delete_marker).count() != 1 {
            return Err("expected one delete marker".into());
        }

        storage
            .purge(&key)
            .await
            .map_err(|error| error.to_string())?;
        if !storage
            .list_versions(&key)
            .await
            .map_err(|error| error.to_string())?
            .is_empty()
        {
            return Err("purge left version entries behind".into());
        }
        Ok(())
    }
    .await;

    let cleanup_result = storage.purge(&key).await;
    delete_bucket(default_s3, &bucket_url).await;

    test_result.unwrap();
    cleanup_result.unwrap();
}

fn bucket_url(endpoint: &Url, bucket: &str) -> Url {
    let mut url = endpoint.clone();
    url.path_segments_mut()
        .expect("S3 endpoint is an origin")
        .pop_if_empty()
        .push(bucket);
    url
}

async fn signed_request(
    config: &S3Config,
    method: Method,
    url: &Url,
    body: &str,
    headers: &[(&str, &str)],
) -> Result<Response, String> {
    let identity = Credentials::new(
        config.access_key.clone(),
        config.secret_key.clone(),
        None,
        None,
        "peppy-storage-version-test",
    )
    .into();
    let mut settings = SigningSettings::default();
    settings.percent_encoding_mode = PercentEncodingMode::Single;
    settings.payload_checksum_kind = PayloadChecksumKind::XAmzSha256;
    settings.uri_path_normalization_mode = UriPathNormalizationMode::Disabled;
    let params = v4::SigningParams::builder()
        .identity(&identity)
        .region(&config.signing_region)
        .name("s3")
        .time(SystemTime::now())
        .settings(settings)
        .build()
        .map_err(|error| error.to_string())?;
    let params: SigningParams<'_> = params.into();
    let signable = SignableRequest::new(
        method.as_str(),
        url.as_str(),
        headers.iter().copied(),
        SignableBody::Bytes(body.as_bytes()),
    )
    .map_err(|error| error.to_string())?;
    let (instructions, _) = sign(signable, &params)
        .map_err(|error| error.to_string())?
        .into_parts();

    let client = Client::builder()
        .no_proxy()
        .build()
        .map_err(|error| error.to_string())?;
    let mut request = client.request(method, url.clone()).body(body.to_owned());
    for (name, value) in headers {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| error.to_string())?;
        let value = HeaderValue::from_str(value).map_err(|error| error.to_string())?;
        request = request.header(name, value);
    }
    for (name, value) in instructions.headers() {
        let name = HeaderName::from_bytes(name.as_bytes()).map_err(|error| error.to_string())?;
        let value = HeaderValue::from_str(value).map_err(|error| error.to_string())?;
        request = request.header(name, value);
    }
    request.send().await.map_err(|error| error.to_string())
}

async fn delete_bucket(config: &S3Config, url: &Url) {
    match signed_request(config, Method::DELETE, url, "", &[]).await {
        Ok(response) if response.status().is_success() || response.status().as_u16() == 404 => {}
        Ok(response) => panic!(
            "versioned bucket cleanup failed: {} {}",
            response.status(),
            response.text().await.unwrap_or_default(),
        ),
        Err(error) => panic!("versioned bucket cleanup request failed: {error}"),
    }
}
