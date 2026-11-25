use crate::common::{ADDRESS, Vault, config, open, route, unlocked};
use peppy_client_core::{ComposeDraftUpdate, Error};
use peppy_domain::DraftId;
use serde_json::Value;

fn text(value: &Value, key: &str) -> String {
    value[key].as_str().unwrap().to_owned()
}

fn update(text: &str, recipients: Vec<String>) -> ComposeDraftUpdate {
    ComposeDraftUpdate {
        text: text.to_owned(),
        recipients,
        ..Default::default()
    }
}

#[test]
fn draft_cas_vectors_match_public_core_api() {
    let fixture: Value = serde_json::from_str(include_str!(
        "../../../packages/contracts/fixtures/draft-cas.json"
    ))
    .unwrap();
    let vectors = fixture["coreVectors"].as_array().unwrap();
    assert!(!vectors.is_empty());
    let vault = Vault::new();
    for vector in vectors {
        let name = text(vector, "name");
        let start = &vector["start"];
        let operation = &vector["operation"];
        let expected = &vector["expected"];
        let dir = tempfile::TempDir::new().unwrap();
        let config = config(&dir, "client", &vault);
        let client = if text(start, "kind") == "retired" {
            unlocked(&config, &vault)
        } else {
            open(&config)
        };
        let kind = text(start, "kind");
        let mut destination = None;
        let initial = match kind.as_str() {
            "absent" => None,
            "new" => Some(client.recipientless_compose_draft().unwrap()),
            "discarded" => {
                let draft = client.recipientless_compose_draft().unwrap();
                client
                    .discard_compose_draft(draft.draft_id, draft.revision)
                    .unwrap();
                Some(draft)
            }
            "retired" => {
                let draft = client.recipientless_compose_draft().unwrap();
                let saved = client
                    .save_compose_draft(
                        draft.draft_id,
                        draft.revision,
                        ComposeDraftUpdate {
                            route: Some(route(&config)),
                            ..update("sent", vec![ADDRESS.into()])
                        },
                    )
                    .unwrap();
                client
                    .send_compose_draft(saved.draft_id, saved.revision)
                    .unwrap();
                let cleared = client.compose_draft(saved.draft_id).unwrap().unwrap();
                let replacement = client
                    .save_compose_draft(
                        cleared.draft_id,
                        cleared.revision,
                        ComposeDraftUpdate {
                            route: cleared.route.clone(),
                            ..update("replacement", cleared.recipients.clone())
                        },
                    )
                    .unwrap();
                assert_ne!(replacement.draft_id, saved.draft_id, "{name}");
                Some(cleared)
            }
            "present" => {
                let draft = client.recipientless_compose_draft().unwrap();
                Some(
                    client
                        .save_compose_draft(
                            draft.draft_id,
                            draft.revision,
                            update(&text(start, "text"), vec![]),
                        )
                        .unwrap(),
                )
            }
            "moved" => {
                let target = client
                    .save_new_compose_draft(update("", vec![ADDRESS.into()]))
                    .unwrap();
                let target = client
                    .save_compose_draft(
                        target.draft_id,
                        target.revision,
                        update("", vec![ADDRESS.into()]),
                    )
                    .unwrap();
                let source = client.recipientless_compose_draft().unwrap();
                let moved = client
                    .save_compose_draft(
                        source.draft_id,
                        source.revision,
                        update(&text(start, "text"), vec![ADDRESS.into()]),
                    )
                    .unwrap();
                assert_eq!(moved.draft_id, target.draft_id, "{name}");
                assert_eq!(
                    moved.revision.to_string(),
                    text(start, "revision"),
                    "{name}"
                );
                destination = Some(moved);
                Some(source)
            }
            _ => panic!("{name}: unknown start kind"),
        };
        if kind == "present" {
            assert_eq!(
                initial.as_ref().unwrap().revision.to_string(),
                text(start, "revision"),
                "{name}"
            );
        }
        let revision: u64 = text(operation, "expectedRevision").parse().unwrap();
        let draft_id = initial
            .as_ref()
            .map(|draft| draft.draft_id)
            .unwrap_or_else(DraftId::new);
        let outcome = match text(operation, "kind").as_str() {
            "create" => {
                assert_eq!(kind, "new", "{name}");
                assert_eq!(revision, 0, "{name}");
                client
                    .save_compose_draft(
                        draft_id,
                        revision,
                        update(&text(operation, "text"), vec![]),
                    )
                    .map(|draft| ("saved", Some(draft)))
            }
            "save" => client
                .save_compose_draft(draft_id, revision, update(&text(operation, "text"), vec![]))
                .map(|draft| ("saved", Some(draft))),
            "discard" => client
                .discard_compose_draft(draft_id, revision)
                .map(|()| ("discarded", None)),
            _ => panic!("{name}: unknown operation"),
        };
        match outcome {
            Ok((actual, draft)) => {
                assert_eq!(actual, text(expected, "kind"), "{name}");
                if let Some(draft) = draft {
                    assert_eq!(
                        draft.revision.to_string(),
                        text(expected, "revision"),
                        "{name}"
                    );
                    assert_eq!(
                        client.compose_draft(draft.draft_id).unwrap().unwrap().text,
                        text(operation, "text"),
                        "{name}"
                    );
                } else {
                    assert!(client.compose_draft(draft_id).unwrap().is_none(), "{name}");
                }
            }
            Err(Error::StaleDraft { current_revision }) => {
                assert_eq!(text(expected, "kind"), "stale-draft", "{name}");
                assert_eq!(
                    current_revision.to_string(),
                    text(expected, "currentRevision"),
                    "{name}"
                );
                let stored = client.compose_draft(draft_id).unwrap();
                if kind == "retired" {
                    assert!(stored.is_none(), "{name}");
                    assert!(
                        matches!(client.save_compose_draft(draft_id, revision, update("retry", vec![])), Err(Error::StaleDraft { current_revision: retry }) if retry == current_revision),
                        "{name}"
                    );
                } else {
                    assert_eq!(stored.as_ref(), initial.as_ref(), "{name}");
                }
            }
            Err(Error::DraftMoved {
                draft_id,
                conversation_id,
                current_revision,
            }) => {
                assert_eq!(text(expected, "kind"), "draft-moved", "{name}");
                assert_eq!(text(expected, "draftId"), "destination", "{name}");
                assert_eq!(text(expected, "conversationId"), "destination", "{name}");
                let target = destination.as_ref().unwrap();
                assert_eq!(draft_id, target.draft_id, "{name}");
                assert_eq!(conversation_id, target.conversation_id, "{name}");
                assert_eq!(
                    current_revision.to_string(),
                    text(expected, "currentRevision"),
                    "{name}"
                );
                assert_eq!(
                    client.compose_draft(draft_id).unwrap().unwrap(),
                    *target,
                    "{name}"
                );
            }
            Err(Error::NotFound) => {
                assert_eq!(text(expected, "kind"), "not-found", "{name}");
                assert!(client.compose_draft(draft_id).unwrap().is_none(), "{name}");
            }
            Err(error) => panic!("{name}: unexpected error {error:?}"),
        }
    }
}
