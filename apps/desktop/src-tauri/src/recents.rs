//! Private, binding-scoped persistence for recently active conversations.

use crate::{credentials::Binding, heads::binding_key};
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, HashSet},
    fs,
    path::{Path, PathBuf},
    sync::Mutex,
};

pub const MAX_RECENTS: usize = 5;
const MAX_RECENT_FILE_BYTES: u64 = 1024 * 1024;
const READ_ERROR: &str = "Could not read recent conversations.";
const WRITE_ERROR: &str = "Could not save recent conversations.";
const INVALID_ID_ERROR: &str = "The conversation is invalid.";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Recent {
    conversation_id: String,
    timestamp: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct RecentFile {
    #[serde(default)]
    bindings: BTreeMap<String, Vec<Recent>>,
}

pub struct RecentStore {
    path: PathBuf,
    transaction: Mutex<()>,
}

impl RecentStore {
    pub fn new(root: &Path) -> Self {
        Self {
            path: root.join("recent-conversations.json"),
            transaction: Mutex::new(()),
        }
    }

    pub fn list(&self, binding: &Binding) -> Result<Vec<String>, String> {
        let _transaction = self
            .transaction
            .lock()
            .map_err(|_| READ_ERROR.to_string())?;
        let file = self.read()?;
        Ok(file
            .bindings
            .get(&binding_key(binding))
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|recent| recent.conversation_id)
            .collect())
    }

    pub fn touch(
        &self,
        binding: &Binding,
        conversation_id: &str,
        timestamp: u64,
    ) -> Result<Vec<String>, String> {
        if !valid_id(conversation_id) {
            return Err(INVALID_ID_ERROR.into());
        }
        let _transaction = self
            .transaction
            .lock()
            .map_err(|_| WRITE_ERROR.to_string())?;
        let mut file = self.read()?;
        let ordered = {
            let recents = file.bindings.entry(binding_key(binding)).or_default();
            recents.retain(|recent| recent.conversation_id != conversation_id);
            let latest = recents
                .iter()
                .map(|recent| recent.timestamp)
                .max()
                .map_or(timestamp, |latest| timestamp.max(latest));
            recents.insert(
                0,
                Recent {
                    conversation_id: conversation_id.to_owned(),
                    timestamp: latest,
                },
            );
            *recents = sanitize(std::mem::take(recents));
            recents
                .iter()
                .map(|recent| recent.conversation_id.clone())
                .collect()
        };
        self.write(&file)?;
        Ok(ordered)
    }

    pub fn remove(&self, binding: &Binding, conversation_id: &str) -> Result<(), String> {
        if !valid_id(conversation_id) {
            return Err(INVALID_ID_ERROR.into());
        }
        let _transaction = self
            .transaction
            .lock()
            .map_err(|_| WRITE_ERROR.to_string())?;
        let mut file = self.read()?;
        let Some(recents) = file.bindings.get_mut(&binding_key(binding)) else {
            return Ok(());
        };
        let prior_len = recents.len();
        recents.retain(|recent| recent.conversation_id != conversation_id);
        if recents.len() != prior_len {
            self.write(&file)?;
        }
        Ok(())
    }

    fn read(&self) -> Result<RecentFile, String> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(RecentFile::default());
            }
            Err(_) => return Err(READ_ERROR.into()),
        };
        if metadata.len() > MAX_RECENT_FILE_BYTES {
            return Err(READ_ERROR.into());
        }
        let bytes = fs::read(&self.path).map_err(|_| READ_ERROR.to_string())?;
        parse_recent_file(&bytes)
    }

    fn write(&self, value: &RecentFile) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|_| WRITE_ERROR.to_string())?;
        if bytes.len() as u64 > MAX_RECENT_FILE_BYTES {
            return Err(WRITE_ERROR.into());
        }
        crate::fsutil::write_private_atomic(&self.path, &bytes).map_err(|_| WRITE_ERROR.into())
    }
}

fn parse_recent_file(bytes: &[u8]) -> Result<RecentFile, String> {
    let value: serde_json::Value =
        serde_json::from_slice(bytes).map_err(|_| READ_ERROR.to_string())?;
    let bindings = value
        .get("bindings")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| READ_ERROR.to_string())?;
    let mut parsed = BTreeMap::new();
    for (binding, records) in bindings {
        let Some(records) = records.as_array() else {
            continue;
        };
        let recents = records
            .iter()
            .filter_map(|record| serde_json::from_value::<Recent>(record.clone()).ok())
            .collect();
        parsed.insert(binding.clone(), sanitize(recents));
    }
    Ok(RecentFile { bindings: parsed })
}

fn sanitize(recents: Vec<Recent>) -> Vec<Recent> {
    let mut seen = HashSet::new();
    let mut recents = recents
        .into_iter()
        .filter(|recent| {
            valid_id(&recent.conversation_id) && seen.insert(recent.conversation_id.clone())
        })
        .collect::<Vec<_>>();
    // Stable sort: ties keep persisted order, which is newest-first by touch.
    recents.sort_by_key(|recent| std::cmp::Reverse(recent.timestamp));
    recents.truncate(MAX_RECENTS);
    recents
}

fn valid_id(conversation_id: &str) -> bool {
    uuid::Uuid::parse_str(conversation_id).is_ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::Binding;
    use std::fs;
    use tempfile::tempdir;

    fn binding(device: &str) -> Binding {
        Binding {
            origin: "https://example.test".into(),
            vault_id: "vault".into(),
            device_id: device.into(),
        }
    }

    fn id() -> String {
        uuid::Uuid::new_v4().to_string()
    }

    #[test]
    fn touches_order_deduplicate_and_cap_recents() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        let ids = (0..=MAX_RECENTS).map(|_| id()).collect::<Vec<_>>();

        for (timestamp, conversation_id) in ids.iter().enumerate() {
            store
                .touch(&binding("one"), conversation_id, timestamp as u64)
                .unwrap();
        }
        store.touch(&binding("one"), &ids[1], 99).unwrap();

        assert_eq!(
            store.list(&binding("one")).unwrap(),
            vec![
                ids[1].clone(),
                ids[5].clone(),
                ids[4].clone(),
                ids[3].clone(),
                ids[2].clone(),
            ]
        );
    }

    #[test]
    fn recents_survive_reopen_and_are_binding_scoped() {
        let root = tempdir().unwrap();
        let first = id();
        let second = id();
        let store = RecentStore::new(root.path());
        store.touch(&binding("one"), &first, 1).unwrap();
        store.touch(&binding("two"), &second, 2).unwrap();
        drop(store);

        let reopened = RecentStore::new(root.path());
        assert_eq!(reopened.list(&binding("one")).unwrap(), vec![first]);
        assert_eq!(reopened.list(&binding("two")).unwrap(), vec![second]);
    }

    #[test]
    fn rejects_invalid_ids() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());

        assert!(store.touch(&binding("one"), "not-a-uuid", 1).is_err());
        assert!(store.remove(&binding("one"), "not-a-uuid").is_err());
    }

    #[test]
    fn malformed_files_fail_closed_without_overwriting_history() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        fs::write(&store.path, b"not json").unwrap();
        let original = fs::read(&store.path).unwrap();

        assert!(store.list(&binding("one")).is_err());
        assert!(store.touch(&binding("one"), &id(), 1).is_err());
        assert!(store.remove(&binding("one"), &id()).is_err());
        assert_eq!(fs::read(&store.path).unwrap(), original);
    }

    #[test]
    fn persistence_contains_no_conversation_names() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        let conversation_id = id();
        store.touch(&binding("one"), &conversation_id, 1).unwrap();

        let contents = fs::read_to_string(&store.path).unwrap();
        assert!(contents.contains(&conversation_id));
        assert!(!contents.contains("name"));
        assert!(!contents.contains("title"));
    }

    #[test]
    fn touch_moves_id_first_when_timestamps_tie() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        let (first, second, third) = (id(), id(), id());
        store.touch(&binding("one"), &first, 5).unwrap();
        store.touch(&binding("one"), &second, 5).unwrap();
        store.touch(&binding("one"), &third, 5).unwrap();
        assert_eq!(
            store.list(&binding("one")).unwrap(),
            vec![third.clone(), second.clone(), first.clone()]
        );

        let ordered = store.touch(&binding("one"), &first, 5).unwrap();
        assert_eq!(ordered, vec![first.clone(), third.clone(), second.clone()]);
        drop(store);

        let reopened = RecentStore::new(root.path());
        assert_eq!(
            reopened.list(&binding("one")).unwrap(),
            vec![first, third, second]
        );
    }

    #[test]
    fn backward_clock_still_moves_touched_id_first_after_reopen() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        let (older, newer) = (id(), id());
        store.touch(&binding("one"), &older, 1_000).unwrap();
        store.touch(&binding("one"), &newer, 500).unwrap();
        drop(store);

        let reopened = RecentStore::new(root.path());
        assert_eq!(
            reopened.list(&binding("one")).unwrap(),
            vec![newer.clone(), older.clone()]
        );
        let ordered = reopened.touch(&binding("one"), &older, 10).unwrap();
        assert_eq!(ordered, vec![older.clone(), newer.clone()]);
        drop(reopened);

        let again = RecentStore::new(root.path());
        assert_eq!(again.list(&binding("one")).unwrap(), vec![older, newer]);
    }

    #[test]
    fn sanitize_drops_invalid_and_duplicate_records_keeping_file_order() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        let (kept, other) = (id(), id());
        let key = binding_key(&binding("one"));
        let contents = serde_json::json!({
            "bindings": {
                key: [
                    { "conversationId": kept, "timestamp": 1 },
                    { "conversationId": "not-a-uuid", "timestamp": 9 },
                    { "conversationId": kept, "timestamp": 8 },
                    { "timestamp": 7 },
                    { "conversationId": other, "timestamp": 1 }
                ]
            }
        });
        fs::write(&store.path, contents.to_string()).unwrap();

        assert_eq!(store.list(&binding("one")).unwrap(), vec![kept, other]);
    }

    #[test]
    fn sanitize_caps_out_of_range_file_records() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        let key = binding_key(&binding("one"));
        let ids = (0..=MAX_RECENTS).map(|_| id()).collect::<Vec<_>>();
        let records = ids
            .iter()
            .enumerate()
            .map(|(index, conversation_id)| {
                serde_json::json!({ "conversationId": conversation_id, "timestamp": index })
            })
            .collect::<Vec<_>>();
        let contents = serde_json::json!({ "bindings": { key: records } });
        fs::write(&store.path, contents.to_string()).unwrap();

        let listed = store.list(&binding("one")).unwrap();
        assert_eq!(listed.len(), MAX_RECENTS);
        assert_eq!(listed[0], ids[MAX_RECENTS]);
    }

    #[test]
    fn remove_drops_confirmed_id_and_persists() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        let (removed, kept) = (id(), id());
        store.touch(&binding("one"), &removed, 2).unwrap();
        store.touch(&binding("one"), &kept, 1).unwrap();

        store.remove(&binding("one"), &removed).unwrap();
        assert_eq!(store.list(&binding("one")).unwrap(), vec![kept.clone()]);
        drop(store);

        let reopened = RecentStore::new(root.path());
        assert_eq!(reopened.list(&binding("one")).unwrap(), vec![kept]);
    }

    #[test]
    fn remove_of_absent_id_leaves_file_untouched() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        store.touch(&binding("one"), &id(), 1).unwrap();
        let original = fs::read(&store.path).unwrap();

        store.remove(&binding("one"), &id()).unwrap();
        store.remove(&binding("two"), &id()).unwrap();
        assert_eq!(fs::read(&store.path).unwrap(), original);
    }

    #[test]
    fn oversized_file_fails_closed_without_overwriting() {
        let root = tempdir().unwrap();
        let store = RecentStore::new(root.path());
        let oversized = vec![b' '; (MAX_RECENT_FILE_BYTES + 1) as usize];
        fs::write(&store.path, &oversized).unwrap();

        assert!(store.list(&binding("one")).is_err());
        assert!(store.touch(&binding("one"), &id(), 1).is_err());
        assert!(store.remove(&binding("one"), &id()).is_err());
        assert_eq!(fs::read(&store.path).unwrap(), oversized);
    }
}
