use std::{
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

use futures_util::{Stream, StreamExt};
use reqwest::{redirect::Policy, Client};
use semver::Version;
use tokio::io::AsyncWriteExt;
use url::Url;
use uuid::Uuid;

use super::{cache::MAX_PAYLOAD_BYTES, types::UpdateChannel};
use crate::error::{BridgeError, BridgeResult};

const MAX_MANIFEST: usize = 1024 * 1024;
const MAX_REDIRECTS: usize = 5;
const METADATA_TIMEOUT: Duration = Duration::from_secs(30);
const PAYLOAD_TIMEOUT: Duration = Duration::from_secs(15 * 60);
const USER_AGENT: &str = "peppy-desktop-updater";
const OWNER: &str = "mattv8";
const REPO: &str = "peppy";
const RELEASES: &str = "https://api.github.com/repos/mattv8/peppy/releases?per_page=100";
const STABLE: &str = "https://github.com/mattv8/peppy/releases/latest/download/latest.json";

pub async fn manifest(channel: UpdateChannel) -> BridgeResult<serde_json::Value> {
    match channel {
        UpdateChannel::Stable => fetch_json(STABLE).await,
        UpdateChannel::Staging => staging().await,
        UpdateChannel::Disabled => Err(BridgeError::new(
            "updates-disabled",
            "Updates are disabled.",
        )),
    }
}

async fn staging() -> BridgeResult<serde_json::Value> {
    let mut releases = Vec::new();
    for page in 1..=5 {
        let url = format!("{RELEASES}&page={page}");
        let page_releases = fetch_json(&url).await?;
        let page_releases = page_releases
            .as_array()
            .ok_or_else(|| BridgeError::new("update-feed", "Invalid staging release list."))?;
        let empty = page_releases.is_empty();
        releases.extend(page_releases.iter().cloned());
        if empty {
            break;
        }
    }
    let tag = select_staging_release(&releases)?;
    let document = fetch_json(&release_asset_url(&tag)).await?;
    validate_staging_manifest(&document, &tag)?;
    Ok(document)
}

fn select_staging_release(releases: &[serde_json::Value]) -> BridgeResult<String> {
    let mut selected: Option<(Version, String)> = None;
    for release in releases {
        if release["draft"].as_bool().unwrap_or(true)
            || release["prerelease"].as_bool() != Some(true)
        {
            continue;
        }
        let Some(tag) = release["tag_name"].as_str() else {
            continue;
        };
        let Ok(version) = tag.strip_prefix('v').unwrap_or(tag).parse::<Version>() else {
            continue;
        };
        if !version.pre.as_str().starts_with("staging.")
            || tag.strip_prefix('v').unwrap_or(tag) != version.to_string()
        {
            continue;
        }
        let exact_asset = release["assets"].as_array().is_some_and(|assets| {
            assets.iter().any(|asset| {
                asset["name"].as_str() == Some("latest.json")
                    && asset["browser_download_url"].as_str() == Some(&release_asset_url(tag))
            })
        });
        if !exact_asset {
            continue;
        }
        if selected.as_ref().is_none_or(|(best, _)| version > *best) {
            selected = Some((version, tag.to_owned()));
        }
    }
    selected
        .map(|(_, tag)| tag)
        .ok_or_else(|| BridgeError::new("update-feed", "No staging update manifest is available."))
}

fn validate_staging_manifest(document: &serde_json::Value, tag: &str) -> BridgeResult<()> {
    let manifest_version = document["version"]
        .as_str()
        .and_then(|value| value.strip_prefix('v').or(Some(value)));
    if manifest_version != Some(tag.strip_prefix('v').unwrap_or(&tag)) {
        Err(BridgeError::new(
            "update-feed",
            "Staging manifest version does not match its release.",
        ))
    } else {
        Ok(())
    }
}

/// Downloads an updater payload through the restricted transport, verifies it after the bounded
/// stream completes, and leaves no partial file on error.
pub async fn download_payload(
    update: &tauri_plugin_updater::Update,
    root: &Path,
    mut progress: impl FnMut(u64, Option<u64>),
) -> BridgeResult<PathBuf> {
    validate_download_url(&update.download_url)?;
    let dir = root.join("updates");
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|_| BridgeError::new("update-cache", "Could not create update cache."))?;
    let partial = dir.join(format!(".payload-{}.partial", Uuid::new_v4()));
    let result = async {
        let response = update_client(PAYLOAD_TIMEOUT)?
            .get(update.download_url.clone())
            .send()
            .await
            .map_err(|_| BridgeError::new("update-payload", "Could not download update payload."))?
            .error_for_status()
            .map_err(|_| BridgeError::new("update-payload", "Update payload request failed."))?;
        let total = response.content_length();
        ensure_payload_length(total)?;
        let mut file = tokio::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&partial)
            .await
            .map_err(|_| BridgeError::new("update-cache", "Could not create update payload."))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            tokio::fs::set_permissions(&partial, std::fs::Permissions::from_mode(0o600))
                .await
                .map_err(|_| {
                    BridgeError::new("update-cache", "Could not secure update payload.")
                })?;
        }
        let size = copy_bounded(
            response.bytes_stream(),
            &mut file,
            MAX_PAYLOAD_BYTES,
            PAYLOAD_TIMEOUT,
            |received| progress(received, total),
        )
        .await?;
        file.flush()
            .await
            .map_err(|_| BridgeError::new("update-cache", "Could not write update payload."))?;
        drop(file);
        let bytes = tokio::fs::read(&partial)
            .await
            .map_err(|_| BridgeError::new("update-cache", "Could not read update payload."))?;
        debug_assert_eq!(size, bytes.len() as u64);
        update.verify(&bytes).map_err(|_| {
            BridgeError::new("update-signature", "Downloaded update verification failed.")
        })?;
        Ok(partial.clone())
    }
    .await;
    if result.is_err() {
        remove_partial(&partial).await;
    }
    result
}

fn ensure_payload_length(total: Option<u64>) -> BridgeResult<()> {
    if total.is_some_and(|size| size > MAX_PAYLOAD_BYTES) {
        Err(BridgeError::new(
            "update-payload",
            "Update payload exceeds the size limit.",
        ))
    } else {
        Ok(())
    }
}

async fn remove_partial(path: &Path) {
    let _ = tokio::fs::remove_file(path).await;
}

async fn fetch_json(value: &str) -> BridgeResult<serde_json::Value> {
    let url =
        Url::parse(value).map_err(|_| BridgeError::new("update-feed", "Invalid update feed."))?;
    validate_manifest_url(&url)?;
    let response = update_client(METADATA_TIMEOUT)?
        .get(url)
        .header("Accept", "application/json")
        .send()
        .await
        .map_err(|_| BridgeError::new("update-feed", "Could not fetch update feed."))?
        .error_for_status()
        .map_err(|_| BridgeError::new("update-feed", "Update feed request failed."))?;
    let bytes = collect_bounded(
        response.bytes_stream(),
        MAX_MANIFEST as u64,
        METADATA_TIMEOUT,
    )
    .await?;
    serde_json::from_slice(&bytes)
        .map_err(|_| BridgeError::new("update-feed", "Invalid update manifest."))
}

fn update_client(timeout: Duration) -> BridgeResult<Client> {
    Client::builder()
        .timeout(timeout)
        .user_agent(USER_AGENT)
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_REDIRECTS {
                return attempt.error("too many redirects");
            }
            if redirect_allowed(attempt.url()) {
                attempt.follow()
            } else {
                attempt.error("untrusted redirect")
            }
        }))
        .build()
        .map_err(|_| BridgeError::new("update-feed", "Could not create update client."))
}

async fn collect_bounded<S, B, E>(
    mut stream: S,
    limit: u64,
    timeout: Duration,
) -> BridgeResult<Vec<u8>>
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
{
    let started = Instant::now();
    let mut output = Vec::new();
    while let Some(chunk) = stream.next().await {
        if started.elapsed() > timeout {
            return Err(BridgeError::new(
                "update-feed",
                "Update manifest request timed out.",
            ));
        }
        let chunk =
            chunk.map_err(|_| BridgeError::new("update-feed", "Update feed interrupted."))?;
        if output.len().saturating_add(chunk.as_ref().len()) > limit as usize {
            return Err(BridgeError::new(
                "update-feed",
                "Update manifest exceeds the size limit.",
            ));
        }
        output.extend_from_slice(chunk.as_ref());
    }
    Ok(output)
}

async fn copy_bounded<S, B, E, W>(
    mut stream: S,
    writer: &mut W,
    limit: u64,
    timeout: Duration,
    mut on_progress: impl FnMut(u64),
) -> BridgeResult<u64>
where
    S: Stream<Item = Result<B, E>> + Unpin,
    B: AsRef<[u8]>,
    W: tokio::io::AsyncWrite + Unpin,
{
    let started = Instant::now();
    let mut received = 0_u64;
    while let Some(chunk) = stream.next().await {
        if started.elapsed() > timeout {
            return Err(BridgeError::new(
                "update-payload",
                "Update download timed out.",
            ));
        }
        let chunk = chunk
            .map_err(|_| BridgeError::new("update-payload", "Update download was interrupted."))?;
        received = received
            .checked_add(chunk.as_ref().len() as u64)
            .filter(|size| *size <= limit)
            .ok_or_else(|| {
                BridgeError::new("update-payload", "Update payload exceeds the size limit.")
            })?;
        writer
            .write_all(chunk.as_ref())
            .await
            .map_err(|_| BridgeError::new("update-cache", "Could not write update payload."))?;
        on_progress(received);
    }
    Ok(received)
}

fn release_asset_url(tag: &str) -> String {
    format!("https://github.com/{OWNER}/{REPO}/releases/download/{tag}/latest.json")
}

fn validate_manifest_url(url: &Url) -> BridgeResult<()> {
    if url.as_str() == STABLE || release_list_url(url) || github_release_asset(url) {
        Ok(())
    } else {
        Err(BridgeError::new(
            "update-feed",
            "Update feed is not trusted.",
        ))
    }
}

fn release_list_url(url: &Url) -> bool {
    let pairs: Vec<_> = url.query_pairs().collect();
    trusted_origin(url, "api.github.com")
        && url.path() == format!("/repos/{OWNER}/{REPO}/releases")
        && pairs
            .iter()
            .any(|(key, value)| key == "per_page" && value == "100")
        && pairs.iter().all(|(key, value)| {
            (key == "per_page" && value == "100")
                || (key == "page"
                    && value
                        .parse::<u8>()
                        .is_ok_and(|page| (1..=5).contains(&page)))
        })
}

fn validate_download_url(url: &Url) -> BridgeResult<()> {
    if github_release_asset(url) {
        Ok(())
    } else {
        Err(BridgeError::new(
            "update-payload",
            "Update payload URL is not trusted.",
        ))
    }
}

fn github_release_asset(url: &Url) -> bool {
    trusted_origin(url, "github.com")
        && url
            .path()
            .starts_with(&format!("/{OWNER}/{REPO}/releases/download/"))
}

fn redirect_allowed(url: &Url) -> bool {
    github_release_asset(url)
        || url.as_str() == STABLE
        || release_list_url(url)
        || [
            "objects.githubusercontent.com",
            "github-releases.githubusercontent.com",
            "release-assets.githubusercontent.com",
        ]
        .iter()
        .any(|host| trusted_origin(url, host))
}

fn trusted_origin(url: &Url, host: &str) -> bool {
    url.scheme() == "https"
        && url.host_str() == Some(host)
        && url.username().is_empty()
        && url.password().is_none()
        && url.port_or_known_default() == Some(443)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_allows_exact_repo_paths_and_https() {
        assert!(validate_download_url(
            &Url::parse("https://github.com/mattv8/peppy/releases/download/v1/a.app").unwrap()
        )
        .is_ok());
        for url in [
            "http://github.com/mattv8/peppy/releases/download/v1/a",
            "https://github.com/other/peppy/releases/download/v1/a",
            "https://github.com/mattv8/peppy/blob/main/a",
            "https://user@github.com/mattv8/peppy/releases/download/v1/a",
        ] {
            assert!(
                validate_download_url(&Url::parse(url).unwrap()).is_err(),
                "{url}"
            );
        }
        assert!(redirect_allowed(
            &Url::parse("https://release-assets.githubusercontent.com/file").unwrap()
        ));
        assert!(!redirect_allowed(
            &Url::parse("https://github.com/other/repo/releases/download/v1/a").unwrap()
        ));
    }

    #[tokio::test]
    async fn bounded_stream_rejects_oversized_response_before_buffering() {
        let chunks = futures_util::stream::iter(vec![
            Ok::<_, ()>(b"1234".to_vec()),
            Ok::<_, ()>(b"5".to_vec()),
        ]);
        assert!(collect_bounded(chunks, 4, Duration::from_secs(1))
            .await
            .is_err());
    }

    #[tokio::test]
    async fn payload_copy_reports_progress_and_keeps_only_bounded_bytes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("payload");
        let mut file = tokio::fs::File::create(&path).await.unwrap();
        let mut progress = Vec::new();
        let chunks = futures_util::stream::iter(vec![
            Ok::<_, ()>(b"abc".to_vec()),
            Ok::<_, ()>(b"de".to_vec()),
        ]);
        assert_eq!(
            copy_bounded(chunks, &mut file, 5, Duration::from_secs(1), |received| {
                progress.push(received)
            })
            .await
            .unwrap(),
            5
        );
        file.flush().await.unwrap();
        assert_eq!(tokio::fs::read(&path).await.unwrap(), b"abcde");
        assert_eq!(progress, vec![3, 5]);
        assert!(ensure_payload_length(Some(MAX_PAYLOAD_BYTES + 1)).is_err());
        assert!(ensure_payload_length(Some(MAX_PAYLOAD_BYTES)).is_ok());
        let overflow = futures_util::stream::iter(vec![
            Ok::<_, ()>(b"abc".to_vec()),
            Ok::<_, ()>(b"def".to_vec()),
        ]);
        let mut capped = tokio::fs::File::create(directory.path().join("capped"))
            .await
            .unwrap();
        assert!(
            copy_bounded(overflow, &mut capped, 5, Duration::from_secs(1), |_| {})
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn partial_cleanup_removes_the_owned_partial() {
        let directory = tempfile::tempdir().unwrap();
        let partial = directory.path().join(".payload-test.partial");
        tokio::fs::write(&partial, b"partial").await.unwrap();
        remove_partial(&partial).await;
        assert!(!partial.exists());
    }

    #[test]
    fn staging_rejects_drafts_bad_assets_and_version_mismatch() {
        let valid = serde_json::json!({"draft":false,"prerelease":true,"tag_name":"v1.2.3-staging.1","assets":[{"name":"latest.json","browser_download_url":"https://github.com/mattv8/peppy/releases/download/v1.2.3-staging.1/latest.json"}]});
        let draft = serde_json::json!({"draft":true,"prerelease":true,"tag_name":"v9.0.0-staging.1","assets":[]});
        assert_eq!(
            select_staging_release(&[draft, valid]).unwrap(),
            "v1.2.3-staging.1"
        );
        assert!(validate_staging_manifest(
            &serde_json::json!({"version":"1.2.3"}),
            "v1.2.3-staging.1"
        )
        .is_err());
    }
}
