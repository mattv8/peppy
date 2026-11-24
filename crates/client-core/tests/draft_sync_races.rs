use crate::common::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
    mpsc,
};
use std::time::Duration;

/// Upper bound for every cross-thread wait, so a missed signal fails the test
/// instead of hanging the run.
const WAIT: Duration = Duration::from_secs(30);

#[test]
fn expiry_restore_retries_after_a_concurrent_attachment_replacement() {
    let dir = tempfile::TempDir::new().unwrap();
    let vault = Vault::new();
    let client = unlocked(&config(&dir, "client", &vault), &vault);
    let old_source = dir.path().join("old.png");
    let added_source = dir.path().join("added.png");
    std::fs::write(&old_source, b"old attachment").unwrap();
    std::fs::write(&added_source, b"added attachment").unwrap();
    let old = client
        .prepare_attachment(&old_source, "image/png", "old.png")
        .unwrap();
    let added = client
        .prepare_attachment(&added_source, "image/png", "added.png")
        .unwrap();
    let initial = client.recipientless_compose_draft().unwrap();
    let saved = client
        .save_compose_draft(
            initial.draft_id,
            initial.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: "before expiry".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![old.attachment_id],
                route: None,
            },
        )
        .unwrap();

    let (prepared_tx, prepared_rx) = mpsc::channel();
    let (resume_tx, resume_rx) = mpsc::channel();
    let target = saved.draft_id;
    let resume_rx = Arc::new(Mutex::new(resume_rx));
    let paused = Arc::new(AtomicBool::new(false));
    let hook = client.install_resurrection_test_hook(
        target,
        Arc::new(move |draft_id| {
            if draft_id == target && !paused.swap(true, Ordering::SeqCst) {
                prepared_tx.send(()).expect("race test: prepared signal");
                resume_rx
                    .lock()
                    .unwrap()
                    .recv_timeout(WAIT)
                    .expect("race test: resume signal timed out");
            }
        }),
    );

    let restoring = client.clone();
    let (restored_tx, restored_rx) = mpsc::channel();
    std::thread::spawn(move || {
        let result = restoring.apply_draft_changes(
            vec![RemoteDraftChange::Terminal {
                draft_id: target,
                reason: DraftTerminalReasonCore::Expired,
                command_id: None,
            }],
            "1",
        );
        let _ = restored_tx.send(result);
    });
    prepared_rx
        .recv_timeout(WAIT)
        .expect("race test: restore never reached the resurrection hook");
    let edited = client
        .save_compose_draft(
            saved.draft_id,
            saved.revision,
            ComposeDraftUpdate {
                group_name: None,
                text: "newer caption".into(),
                recipients: vec![ADDRESS.into()],
                attachment_ids: vec![added.attachment_id],
                route: None,
            },
        )
        .unwrap();
    resume_tx.send(()).expect("race test: resume signal");
    restored_rx
        .recv_timeout(WAIT)
        .expect("race test: restore did not finish after resume")
        .unwrap();
    drop(hook);

    assert!(client.compose_draft(saved.draft_id).unwrap().is_none());
    let restored = client.compose_drafts().unwrap().pop().unwrap();
    assert_eq!(restored.text, "newer caption");
    assert_eq!(restored.recipients, vec![ADDRESS]);
    assert_eq!(restored.attachment_ids.len(), 1);
    assert_ne!(restored.attachment_ids[0], old.attachment_id);
    assert_ne!(restored.attachment_ids[0], added.attachment_id);
    assert_eq!(client.draft_sync_cursor().unwrap(), "1");
    assert_eq!(edited.revision, saved.revision + 1);
    assert!(client.attachment_info(added.attachment_id).is_err());
    let media_dir = client.native_cipher_file(old.attachment_id).unwrap();
    assert_eq!(
        std::fs::read_dir(media_dir.parent().unwrap())
            .unwrap()
            .count(),
        2
    );
}
