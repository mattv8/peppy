use std::{
    fs,
    net::TcpListener,
    path::Path,
    process::{Child, Command, Stdio},
    time::{Duration, Instant},
};

use peppy_mcp::registry::StateLock;

struct WorkerProcess(Child);

impl Drop for WorkerProcess {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn_worker(root: &Path, address: &str) -> WorkerProcess {
    WorkerProcess(
        Command::new(env!("CARGO_BIN_EXE_peppy-mcp"))
            .env("PUBLIC_API_URL", "https://api.example.test")
            .env("PEPPY_MCP_API_URL", "http://127.0.0.1:1")
            .env("PEPPY_MCP_STATE_DIR", root.join("state"))
            .env("PEPPY_MCP_KEY_FILE", root.join("keys/storage-key"))
            .env(
                "PEPPY_MCP_WORKER_TOKEN_FILE",
                root.join("control/control-token"),
            )
            .env("PEPPY_MCP_BIND_ADDR", address)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    )
}

async fn start_ready(root: &Path) -> WorkerProcess {
    let reservation = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let mut worker = spawn_worker(root, &address.to_string());
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        assert!(
            worker.0.try_wait().unwrap().is_none(),
            "worker exited before readiness"
        );
        if let Ok(response) = client.get(format!("http://{address}/healthz")).send().await {
            assert!(response.status().is_success());
            assert_eq!(response.text().await.unwrap(), "ok");
            return worker;
        }
        assert!(Instant::now() < deadline, "worker readiness timed out");
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn empty_startup_creates_private_distinct_secrets_and_restart_preserves_them() {
    let root = tempfile::tempdir().unwrap();
    for directory in ["state", "control", "keys"] {
        fs::create_dir(root.path().join(directory)).unwrap();
    }
    let worker = start_ready(root.path()).await;
    let token_path = root.path().join("control/control-token");
    let key_path = root.path().join("keys/storage-key");
    let token = fs::read(&token_path).unwrap();
    let key = fs::read(&key_path).unwrap();
    assert!(token.len() >= 43);
    assert_eq!(key.len(), 32);
    assert!(token != key, "control token must not be the custody KEK");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&token_path, &key_path] {
            assert_eq!(
                fs::metadata(path).unwrap().permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(path.parent().unwrap())
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
        }
    }
    drop(worker);
    let _restarted = start_ready(root.path()).await;
    assert!(
        fs::read(token_path).unwrap() == token,
        "restart changed control token"
    );
    assert!(
        fs::read(key_path).unwrap() == key,
        "restart changed custody KEK"
    );
}

#[test]
fn process_lock_prevents_token_initialization() {
    let root = tempfile::tempdir().unwrap();
    let _lock = StateLock::acquire(&root.path().join("state")).unwrap();
    let mut worker = spawn_worker(root.path(), "127.0.0.1:0");
    assert!(!worker.0.wait().unwrap().success());
    assert!(!root.path().join("control/control-token").exists());
    assert!(!root.path().join("keys/storage-key").exists());
}
