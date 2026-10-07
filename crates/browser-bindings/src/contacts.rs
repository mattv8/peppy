use super::*;
use peppy_desktop_api::{ContactError, contacts as shared};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, OpenOptions},
    io::Write,
    time::{SystemTime, UNIX_EPOCH},
};

pub(super) fn handles(command: &str) -> bool {
    matches!(
        command,
        "list_contact_books"
            | "forget_contact_book"
            | "list_contacts"
            | "submit_contact_edit"
            | "list_contact_edits"
            | "search_contact_recipients"
            | "list_restorable_contacts"
            | "restore_contact"
            | "request_contact_repair"
            | "contact_repair_status"
            | "contact_snapshot"
    )
}

fn contact_error(error: ContactError) -> Failure {
    match error {
        ContactError::Ui { code, message } => Failure::new(code, message),
        ContactError::Core(error) => core(error),
    }
}
impl From<ContactError> for Failure {
    fn from(error: ContactError) -> Self {
        contact_error(error)
    }
}

fn parse(result: Result<String, CoreError>) -> Result<Value, Failure> {
    serde_json::from_str(&result.map_err(core)?).map_err(|_| core(CoreError::Database))
}

fn now_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

struct ContactSnapshot {
    resolution: Option<serde_json::Map<String, Value>>,
    books: Option<Vec<Value>>,
    pending_count: Option<u64>,
    sync: Value,
}

impl BrowserCore {
    pub(super) fn contacts_command(&self, command: &str, args: Value) -> Result<Value, Failure> {
        match command {
            "list_contact_books" => self.contact_books(),
            "forget_contact_book" => self.forget_contact_book(args),
            "list_contacts" => self.list_contacts(args),
            "submit_contact_edit" => self.submit_contact_edit(args),
            "list_contact_edits" => self.list_contact_edits(args),
            "search_contact_recipients" => self.search_contact_recipients(args),
            "list_restorable_contacts" => self.list_restorable_contacts(args),
            "restore_contact" => self.restore_contact(args),
            "request_contact_repair" => self.request_contact_repair(),
            "contact_repair_status" => self.contact_repair_status(),
            "contact_snapshot" => self.contact_snapshot(args),
            _ => Err(unknown_command()),
        }
    }

    fn own_device_id(&self) -> Result<&DeviceId, Failure> {
        self.device_id.as_ref().ok_or_else(unavailable)
    }

    fn raw_books(&self) -> Result<Vec<Value>, Failure> {
        let raw = parse(self.client()?.list_contact_books_json())?;
        raw.get("books")
            .and_then(Value::as_array)
            .cloned()
            .ok_or_else(|| core(CoreError::Database))
    }

    /// Requester rows are queried with this browser's own device ID, then filtered locally.
    fn ledger(&self, book_id: Option<&str>) -> Result<Vec<Value>, Failure> {
        let requester = self.own_device_id()?.to_string();
        let raw = parse(self.client()?.list_contact_requests_json(
            &json!({"schema_version": 1, "requester": requester}).to_string(),
        ))?;
        shared::requester_ledger_view(&raw, book_id, now_seconds()).map_err(contact_error)
    }

    fn contact_books(&self) -> Result<Value, Failure> {
        let pending = shared::pending_counts(&self.ledger(None)?);
        let own = self.own_device_id()?.to_string();
        Ok(Value::Array(
            self.raw_books()?
                .iter()
                .filter_map(|book| {
                    shared::book_view(book, &own, &std::collections::HashMap::new(), &pending)
                })
                .collect(),
        ))
    }

    /// Display-only contact state for a browser snapshot. No provider data, photo attachment ID,
    /// or local path leaves the core.
    fn snapshot_contacts(
        &self,
        addresses: &[(String, Option<String>)],
    ) -> Result<ContactSnapshot, Failure> {
        const MAX_ADDRESSES: usize = 1000;
        const RESOLVE_BATCH: usize = 100;
        let mut by_source: HashMap<Option<&str>, Vec<&str>> = HashMap::new();
        let mut seen = HashSet::new();
        for (address, source) in addresses {
            let digits = address.bytes().filter(u8::is_ascii_digit).count();
            let phone = (3..=15).contains(&digits)
                && address.bytes().all(|byte| {
                    byte.is_ascii_digit() || matches!(byte, b'+' | b' ' | b'(' | b')' | b'.' | b'-')
                });
            if address.len() <= 256 && phone && seen.insert(address.as_str()) {
                by_source
                    .entry(source.as_deref().filter(|id| shared::valid_id(id)))
                    .or_default()
                    .push(address);
            }
            if seen.len() >= MAX_ADDRESSES {
                break;
            }
        }
        let mut resolution = serde_json::Map::new();
        for (source, addresses) in by_source {
            for batch in addresses.chunks(RESOLVE_BATCH) {
                let mut input = json!({"addresses": batch});
                if let Some(source) = source {
                    input["source_device_id"] = json!(source);
                }
                let raw = parse(
                    self.client()?
                        .resolve_contact_addresses_json(&input.to_string()),
                )?;
                for (address, contact) in shared::resolved_addresses_view(&raw) {
                    resolution.entry(address).or_insert_with(|| {
                        json!({
                            "contactId": contact.contact_id,
                            "bookId": contact.book_id,
                            "displayName": contact.display_name,
                        })
                    });
                }
            }
        }
        let books = self
            .contact_books()
            .ok()
            .and_then(|value| value.as_array().cloned());
        let pending = books.as_ref().map(|books| {
            books
                .iter()
                .filter_map(|book| book.get("pendingEditCount").and_then(Value::as_u64))
                .sum()
        });
        let readiness = self
            .client()?
            .contact_sync_readiness_json()
            .ok()
            .and_then(|value| serde_json::from_str(&value).ok());
        let projection = self
            .client()?
            .snapshot_projection_status()
            .ok()
            .flatten()
            .map(|status| {
                use peppy_client_core::SnapshotProjectionState as State;
                let state = match status.state {
                    State::Draining | State::Staging => "rebuilding",
                    State::Promoted => "current",
                    State::Failed => "failed",
                };
                (state, status.reason)
            });
        let sync = shared::sync_status(
            self.client()?.contact_repair_required().unwrap_or(false),
            readiness,
            projection
                .as_ref()
                .map(|(state, reason)| (*state, reason.as_deref())),
        );
        Ok(ContactSnapshot {
            resolution: (!resolution.is_empty()).then_some(resolution),
            books,
            pending_count: pending,
            sync,
        })
    }

    fn contact_snapshot(&self, args: Value) -> Result<Value, Failure> {
        let addresses = args
            .get("addresses")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?
            .iter()
            .map(|item| {
                Ok((
                    item.get("address")
                        .and_then(Value::as_str)
                        .ok_or_else(invalid)?
                        .to_owned(),
                    item.get("sourceDeviceId")
                        .and_then(Value::as_str)
                        .map(str::to_owned),
                ))
            })
            .collect::<Result<Vec<_>, Failure>>()?;
        let snapshot = self.snapshot_contacts(&addresses)?;
        Ok(json!({
            "contactResolution": snapshot.resolution,
            "contactBooks": snapshot.books,
            "contactsPendingCount": snapshot.pending_count,
            "contactSync": snapshot.sync,
        }))
    }

    fn find_book(&self, book_id: &str) -> Result<Value, Failure> {
        self.raw_books()?
            .into_iter()
            .find(|book| book.get("id").and_then(Value::as_str) == Some(book_id))
            .ok_or_else(|| Failure::new("not-found", "The contact book was not found."))
    }

    fn forget_contact_book(&self, args: Value) -> Result<Value, Failure> {
        let book_id = args
            .get("bookId")
            .and_then(Value::as_str)
            .filter(|id| shared::valid_id(id))
            .ok_or_else(|| Failure::new("invalid-contact-edit", "The contact book is invalid."))?;
        parse(
            self.client()?
                .forget_contact_book(&json!({"book_id": book_id}).to_string()),
        )?;
        Ok(json!({}))
    }

    fn list_contacts(&self, args: Value) -> Result<Value, Failure> {
        let book_id = args
            .get("bookId")
            .and_then(Value::as_str)
            .filter(|id| shared::valid_id(id))
            .ok_or_else(|| Failure::new("invalid-contact-edit", "The contact book is invalid."))?;
        let query = args.get("query").and_then(Value::as_str).unwrap_or("");
        if query.chars().count() > 1024 {
            return Err(Failure::new(
                "invalid-contact-edit",
                "The contact search is too long.",
            ));
        }
        let offset = args.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let raw = parse(self.client()?.contact_book_view(&json!({"schema_version": 1, "book_id": book_id, "search": query, "limit": shared::PAGE_LIMIT, "offset": offset}).to_string()))?;
        let contacts = raw
            .get("contacts")
            .and_then(Value::as_array)
            .ok_or_else(|| core(CoreError::Database))?;
        let badges = shared::contact_badges(&self.ledger(Some(book_id))?, now_seconds());
        let mut no_avatar = |_| None;
        Ok(Value::Array(
            contacts
                .iter()
                .filter_map(|contact| {
                    let mut view = shared::contact_view(contact, book_id, &mut no_avatar)?;
                    if let Some(edit) = view
                        .get("id")
                        .and_then(Value::as_str)
                        .and_then(|id| badges.get(id))
                    {
                        shared::apply_badge(&mut view, edit);
                    }
                    Some(view)
                })
                .collect(),
        ))
    }

    fn prepare_browser_contact_photo(&self, bytes: &[u8]) -> Result<AttachmentId, Failure> {
        let dir = self.root.join("contact-photo-staging");
        fs::create_dir_all(&dir).map_err(|_| {
            Failure::new(
                "attachment-local",
                "Local attachment storage is unavailable.",
            )
        })?;
        let path = dir.join(format!("{}.img", Uuid::new_v4().simple()));
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .map_err(|_| {
                Failure::new(
                    "attachment-local",
                    "Local attachment storage is unavailable.",
                )
            })?;
        let write = file.write_all(bytes).and_then(|_| file.sync_all());
        drop(file);
        if write.is_err() {
            let _ = fs::remove_file(&path);
            return Err(Failure::new(
                "attachment-local",
                "Local attachment storage is unavailable.",
            ));
        }
        let result = self.client()?.prepare_contact_photo(&path).map_err(core);
        let _ = fs::remove_file(path);
        result.map(|info| info.attachment_id)
    }

    fn submit_contact_edit(&self, args: Value) -> Result<Value, Failure> {
        let book_id = args
            .get("targetBookId")
            .and_then(Value::as_str)
            .ok_or_else(invalid)?;
        let book = self.find_book(book_id)?;
        let client = self.client()?;
        shared::submit_with(
            &args,
            &book,
            &self.own_device_id()?.to_string(),
            |bytes| self.prepare_browser_contact_photo(bytes),
            |request| parse(client.request_contact_edit(&request.to_string())),
            |id| {
                let _ = client.discard_unreferenced_attachment(id);
            },
        )
    }

    fn list_contact_edits(&self, args: Value) -> Result<Value, Failure> {
        let book = args.get("bookId").and_then(Value::as_str);
        Ok(Value::Array(self.ledger(book)?))
    }

    fn search_contact_recipients(&self, args: Value) -> Result<Value, Failure> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim();
        if query.is_empty() {
            return Ok(json!([]));
        }
        if query.chars().count() > 128 {
            return Err(Failure::new(
                "invalid-contact-edit",
                "The contact search is too long.",
            ));
        }
        let mut input =
            json!({"schema_version": 1, "query": query, "limit": shared::MAX_SEARCH_RESULTS});
        if let Some(source) = args
            .get("sourceDeviceId")
            .and_then(Value::as_str)
            .filter(|id| shared::valid_id(id))
        {
            input["source_device_id"] = json!(source);
        }
        let raw = parse(
            self.client()?
                .search_contact_recipients_json(&input.to_string()),
        )?;
        let mut no_avatar = |_| None;
        Ok(Value::Array(
            raw.get("results")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|row| shared::recipient_view(row, &mut no_avatar))
                .collect(),
        ))
    }

    fn list_restorable_contacts(&self, args: Value) -> Result<Value, Failure> {
        let book_id = args
            .get("bookId")
            .and_then(Value::as_str)
            .filter(|id| shared::valid_id(id))
            .ok_or_else(|| Failure::new("invalid-contact-edit", "The contact book is invalid."))?;
        let raw = parse(
            self.client()?
                .list_restorable_contacts_json(&json!({"book_id": book_id}).to_string()),
        )?;
        let mut no_avatar = |_| None;
        Ok(Value::Array(
            raw.get("contacts")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|contact| shared::restorable_view(contact, book_id, &mut no_avatar))
                .collect(),
        ))
    }

    fn restore_contact(&self, args: Value) -> Result<Value, Failure> {
        let book_id = args
            .get("bookId")
            .and_then(Value::as_str)
            .filter(|id| shared::valid_id(id))
            .ok_or_else(|| Failure::new("invalid-contact-edit", "The contact is invalid."))?;
        let contact_id = args
            .get("contactId")
            .and_then(Value::as_str)
            .filter(|id| shared::valid_id(id))
            .ok_or_else(|| Failure::new("invalid-contact-edit", "The contact is invalid."))?;
        let book = self.find_book(book_id)?;
        if !shared::capabilities(&book, &self.own_device_id()?.to_string()).can_restore {
            return Err(Failure::new(
                "contact-book-read-only",
                "This contact book does not accept changes from this computer.",
            ));
        }
        let result = parse(self.client()?.restore_contact_json(
            &json!({"book_id": book_id, "contact_id": contact_id}).to_string(),
        ))?;
        let request_id = result
            .get("request_id")
            .and_then(Value::as_str)
            .ok_or_else(|| core(CoreError::Database))?;
        Ok(json!({"state": "pending", "requestId": request_id}))
    }

    fn request_contact_repair(&self) -> Result<Value, Failure> {
        let client = self.client()?;
        client.request_contact_repair().map_err(core)?;
        self.contact_repair_status()
    }

    fn contact_repair_status(&self) -> Result<Value, Failure> {
        let client = self.client()?;
        let readiness = client
            .contact_sync_readiness_json()
            .ok()
            .and_then(|json| serde_json::from_str(&json).ok());
        let projection = client
            .snapshot_projection_status()
            .ok()
            .flatten()
            .map(|status| {
                let state = match status.state {
                    peppy_client_core::SnapshotProjectionState::Draining
                    | peppy_client_core::SnapshotProjectionState::Staging => "rebuilding",
                    peppy_client_core::SnapshotProjectionState::Promoted => "current",
                    peppy_client_core::SnapshotProjectionState::Failed => "failed",
                };
                (state, status.reason)
            });
        Ok(shared::sync_status(
            client.contact_repair_required().unwrap_or(true),
            readiness,
            projection
                .as_ref()
                .map(|(state, reason)| (*state, reason.as_deref())),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use peppy_client_core::{ApplyReport, Cursor};
    use peppy_crypto::{create_vault_check_header, derive_root_key};
    use std::collections::BTreeSet;
    use tempfile::TempDir;

    const PASSPHRASE: &str = "browser contact contract fixture";
    const BOOK_ID: &str = "phone-book";
    const READ_ONLY_BOOK_ID: &str = "read-only-book";
    const OTHER_BOOK_ID: &str = "other-phone-book";

    struct ContactFixture {
        _root: TempDir,
        owner: Client,
        owner_config: ClientConfig,
        browser: BrowserCore,
        owner_to_browser_cursor: u64,
        browser_to_owner_cursor: u64,
    }

    impl ContactFixture {
        fn new() -> Self {
            let root = tempfile::tempdir().unwrap();
            let vault_id = VaultId::new();
            let owner_device = DeviceId::new();
            let browser_device = DeviceId::new();
            let profile = KeyProfile::new(vault_id.0, 1).unwrap();
            let root_key = derive_root_key(PASSPHRASE, &profile).unwrap();
            let header = create_vault_check_header(&root_key, profile.clone()).unwrap();
            let owner_config = ClientConfig {
                database_path: root.path().join("owner.db"),
                vault_id,
                device_id: owner_device,
            };
            let owner =
                Client::open(owner_config.clone(), DatabaseKey::new(&[31; 32]).unwrap()).unwrap();
            owner.unlock(&profile, &header, PASSPHRASE).unwrap();
            owner.set_server_compaction_state(true, true).unwrap();

            let browser_root = root.path().join("browser");
            fs::create_dir_all(&browser_root).unwrap();
            let mut browser = BrowserCore::new(browser_root);
            dispatch_value(
                &mut browser,
                "open",
                json!({
                    "vaultId": vault_id,
                    "deviceId": browser_device,
                    "databaseKey": vec![47; 32],
                    "origin": "https://peppy.test/",
                    "deviceRole": "device",
                }),
            );
            dispatch_value(
                &mut browser,
                "unlock",
                json!({"profile": profile, "header": header, "passphrase": PASSPHRASE}),
            );
            browser
                .client()
                .unwrap()
                .set_server_compaction_state(true, true)
                .unwrap();

            let mut fixture = Self {
                _root: root,
                owner,
                owner_config,
                browser,
                owner_to_browser_cursor: 0,
                browser_to_owner_cursor: 0,
            };
            fixture.capture_book(
                BOOK_ID,
                true,
                vec![
                    contact("c1", "Ada", "+12025550100"),
                    contact("c2", "Grace", "+12025550101"),
                    contact("c3", "Katherine", "+12025550102"),
                ],
            );
            fixture.capture_book(OTHER_BOOK_ID, true, Vec::new());
            fixture.capture_book(
                READ_ONLY_BOOK_ID,
                false,
                vec![contact_in_book(
                    READ_ONLY_BOOK_ID,
                    "locked-contact",
                    "Readonly",
                    "+12025550103",
                )],
            );
            fixture.deliver_owner_to_browser();
            fixture
        }

        fn capture_book(&self, id: &str, writable: bool, contacts: Vec<Value>) {
            let input = json!({
                "schema_version": 1,
                "book": {
                    "id": id,
                    "owner_device_id": self.owner_config.device_id,
                    "generation": "1",
                    "state": "active",
                    "capabilities": {
                        "read": true,
                        "write": writable,
                        "photo": writable,
                        "notes": true,
                        "birthday": true,
                    },
                    "accounts": [{
                        "id": "native:secret-account",
                        "name": "Phone contacts",
                        "writable": writable,
                    }],
                    "default_account_id": "native:secret-account",
                    "policy": {"remote_edits": "auto", "large_delete_requires_approval": true},
                },
                "contacts": contacts,
            });
            self.owner.capture_contact_book(&input.to_string()).unwrap();
        }

        fn deliver_owner_to_browser(&mut self) -> ApplyReport {
            let envelopes = self.owner.pending_outbox().unwrap();
            for envelope in &envelopes {
                self.owner_to_browser_cursor += 1;
                self.browser
                    .client()
                    .unwrap()
                    .ingest(envelope, Cursor(self.owner_to_browser_cursor))
                    .unwrap();
                self.owner.ack_outbox(envelope.envelope_id).unwrap();
            }
            self.browser.client().unwrap().apply_pending(100).unwrap()
        }

        fn deliver_browser_to_owner(&mut self) -> ApplyReport {
            let envelopes = self.browser.client().unwrap().pending_outbox().unwrap();
            for envelope in &envelopes {
                self.browser_to_owner_cursor += 1;
                self.owner
                    .ingest(envelope, Cursor(self.browser_to_owner_cursor))
                    .unwrap();
                self.browser
                    .client()
                    .unwrap()
                    .ack_outbox(envelope.envelope_id)
                    .unwrap();
            }
            self.owner.apply_pending(100).unwrap()
        }

        fn command(&mut self, command: &str, args: Value) -> Value {
            dispatch_value(&mut self.browser, command, args)
        }

        fn error(&mut self, command: &str, args: Value) -> Value {
            dispatch_error(&mut self.browser, command, args)
        }
    }

    fn contact(id: &str, given: &str, phone: &str) -> Value {
        contact_in_book(BOOK_ID, id, given, phone)
    }

    fn contact_in_book(book_id: &str, id: &str, given: &str, phone: &str) -> Value {
        json!({
            "id": id,
            "book_id": book_id,
            "revision": "999",
            "display_name": format!("{given} Lovelace"),
            "name": {"given": given, "family": "Lovelace"},
            "nickname": format!("{given} private-path /native/contact"),
            "phones": [{
                "id": format!("phone-{id}"),
                "label": "_$!<Mobile>!$_",
                "value": phone,
            }],
            "emails": [],
            "addresses": [],
            "provenance": {
                "source_id": format!("native:secret-{id}"),
                "account_id": "native:secret-account",
                "read_only": false,
            },
        })
    }

    fn dispatch_value(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        let response: Value = serde_json::from_str(
            &core.dispatch(&json!({"command": command, "args": args}).to_string()),
        )
        .unwrap();
        assert_eq!(response["ok"], true, "{response}");
        response["value"].clone()
    }

    fn dispatch_error(core: &mut BrowserCore, command: &str, args: Value) -> Value {
        let response: Value = serde_json::from_str(
            &core.dispatch(&json!({"command": command, "args": args}).to_string()),
        )
        .unwrap();
        assert_eq!(response["ok"], false, "{response}");
        response["error"].clone()
    }

    fn object_keys(value: &Value) -> BTreeSet<&str> {
        value
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect()
    }

    fn assert_staging_empty(fixture: &ContactFixture) {
        let path = fixture.browser.root.join("contact-photo-staging");
        if path.exists() {
            assert_eq!(fs::read_dir(path).unwrap().count(), 0);
        }
    }

    #[test]
    fn actual_sqlcipher_book_list_and_contact_search_use_camel_case_whitelists() {
        let mut fixture = ContactFixture::new();
        let books = fixture.command("list_contact_books", json!({}));
        assert_eq!(books.as_array().unwrap().len(), 3);
        let book = books
            .as_array()
            .unwrap()
            .iter()
            .find(|book| book["id"] == BOOK_ID)
            .unwrap();
        assert_eq!(book["deviceName"], "Phone");
        assert_eq!(book["contactCount"], 3);
        assert_eq!(book["capabilities"]["canWrite"], true);
        assert_eq!(book["capabilities"]["supportsPhoto"], true);
        assert_eq!(
            object_keys(book),
            BTreeSet::from([
                "capabilities",
                "contactCount",
                "defaultAccountLabel",
                "deviceName",
                "id",
                "pendingEditCount",
                "state",
            ])
        );

        let contacts = fixture.command(
            "list_contacts",
            json!({"bookId": BOOK_ID, "query": "lovelace", "offset": 1}),
        );
        assert_eq!(contacts.as_array().unwrap().len(), 2);
        assert_eq!(contacts[0]["id"], "c2");
        assert_eq!(contacts[0]["bookId"], BOOK_ID);
        assert_eq!(contacts[0]["displayName"], "Grace Lovelace");
        assert_eq!(contacts[0]["phones"][0]["id"], "phone-c2");
        assert_eq!(contacts[0]["phones"][0]["displayLabel"], "mobile");
        let snapshot = fixture.command(
            "contact_snapshot",
            json!({"addresses": [{"address": "+12025550100"}]}),
        );
        assert_eq!(
            snapshot["contactResolution"]["+12025550100"]["displayName"],
            "Ada Lovelace"
        );
        assert_eq!(snapshot["contactBooks"].as_array().unwrap().len(), 3);
        assert!(snapshot["contactSync"].is_object());
        let serialized = json!({"books": books, "contacts": contacts}).to_string();
        for forbidden in [
            "owner_device_id",
            "provider_id",
            "file_key",
            "photo_attachment_id",
            "attachment_id",
            "native:secret",
        ] {
            assert!(
                !serialized.contains(forbidden),
                "leaked {forbidden}: {serialized}"
            );
        }

        let recipients = fixture.command(
            "search_contact_recipients",
            json!({"query": "ada", "sourceDeviceId": fixture.owner_config.device_id}),
        );
        assert_eq!(recipients.as_array().unwrap().len(), 1);
        assert_eq!(
            object_keys(&recipients[0]),
            BTreeSet::from([
                "address",
                "contactId",
                "displayName",
                "label",
                "normalized",
                "number",
                "phoneId",
            ])
        );
        assert_eq!(recipients[0]["address"], "+12025550100");
        assert_eq!(recipients[0]["label"], "mobile");
        assert!(!recipients.to_string().contains("photo_attachment_id"));

        assert_eq!(
            fixture.error(
                "list_contacts",
                json!({"bookId": BOOK_ID, "query": "x".repeat(1025)}),
            )["code"],
            "invalid-contact-edit"
        );
        assert_eq!(
            fixture.error(
                "search_contact_recipients",
                json!({"query": "x".repeat(129)}),
            )["code"],
            "invalid-contact-edit"
        );
    }

    #[test]
    fn edit_request_keeps_field_ids_and_waits_for_encrypted_owner_result() {
        let mut fixture = ContactFixture::new();
        let contacts = fixture.command(
            "list_contacts",
            json!({"bookId": BOOK_ID, "query": "ada", "offset": 0}),
        );
        let original_phones = contacts[0]["phones"].clone();
        let mut changed_phones = original_phones.clone();
        changed_phones[0]["number"] = json!("+12025550999");
        let submitted = fixture.command(
            "submit_contact_edit",
            json!({
                "targetBookId": BOOK_ID,
                "kind": "update",
                "contactId": "c1",
                "baseRevision": contacts[0]["revision"],
                "patches": [{
                    "field": "phones",
                    "expectedOld": original_phones,
                    "value": changed_phones,
                }],
                "photo": {"kind": "keep"},
            }),
        );
        assert_eq!(submitted["state"], "pending");
        let request_id = submitted["requestId"].as_str().unwrap().to_owned();
        let own_ledger = fixture.command("list_contact_edits", json!({"bookId": BOOK_ID}));
        assert_eq!(own_ledger.as_array().unwrap().len(), 1);
        assert_eq!(own_ledger[0]["requestId"], request_id);
        assert_eq!(own_ledger[0]["bookId"], BOOK_ID);
        assert_eq!(own_ledger[0]["state"], "pending");
        let other = fixture.command(
            "submit_contact_edit",
            json!({
                "targetBookId": OTHER_BOOK_ID,
                "kind": "create",
                "patches": [{"field": "givenName", "value": "Other"}],
            }),
        );
        assert_eq!(other["state"], "pending");
        let filtered = fixture.command("list_contact_edits", json!({"bookId": BOOK_ID}));
        assert_eq!(filtered.as_array().unwrap().len(), 1);
        assert_eq!(filtered[0]["requestId"], request_id);

        fixture.deliver_browser_to_owner();
        let permit: Value = serde_json::from_str(
            &fixture
                .owner
                .next_contact_apply_permit(
                    &json!({"schema_version": 1, "request_id": request_id}).to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(permit["status"], "permit");
        assert_eq!(permit["request"]["patches"][0]["path"], "phones[phone-c1]");
        assert_eq!(
            permit["request"]["expected_old"]["phones"][0]["id"],
            "phone-c1"
        );
        assert_eq!(
            fixture.command("list_contact_edits", json!({"bookId": BOOK_ID}))[0]["state"],
            "pending",
            "owner receipt and permit are not an applied result"
        );

        let observed = contact("c1", "Ada", "+12025550999");
        let result: Value = serde_json::from_str(
            &fixture
                .owner
                .reconcile_contact_apply(
                    &json!({
                        "schema_version": 1,
                        "request_id": request_id,
                        "outcome": "applied",
                        "observed": observed,
                    })
                    .to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(result["status"], "applied");
        fixture.deliver_owner_to_browser();
        assert_eq!(
            fixture.command("list_contact_edits", json!({"bookId": BOOK_ID}))[0]["state"],
            "applied"
        );

        assert_eq!(
            fixture.error(
                "submit_contact_edit",
                json!({
                    "targetBookId": READ_ONLY_BOOK_ID,
                    "kind": "delete",
                    "contactId": "locked-contact",
                    "baseRevision": "1",
                }),
            )["code"],
            "contact-book-read-only"
        );
        assert_eq!(
            fixture.error(
                "submit_contact_edit",
                json!({
                    "targetBookId": "unknown-book",
                    "kind": "delete",
                    "contactId": "c1",
                    "baseRevision": "1",
                }),
            )["code"],
            "not-found"
        );
    }

    #[test]
    fn ledger_excludes_a_real_encrypted_request_from_another_requester() {
        let mut fixture = ContactFixture::new();
        let browser_device = *fixture.browser.device_id.as_ref().unwrap();
        fixture
            .browser
            .client()
            .unwrap()
            .capture_contact_book(
                &json!({
                    "schema_version": 1,
                    "book": {
                        "id": "browser-owned-book",
                        "owner_device_id": browser_device,
                        "generation": "1",
                        "state": "active",
                        "capabilities": {"read": true, "write": true, "photo": false},
                        "policy": {"remote_edits": "auto", "large_delete_requires_approval": true},
                    },
                    "contacts": [],
                })
                .to_string(),
            )
            .unwrap();
        fixture.deliver_browser_to_owner();
        fixture
            .owner
            .request_contact_edit(
                &json!({
                    "schema_version": 1,
                    "request_id": "foreign-request",
                    "target_owner": browser_device,
                    "book_id": "browser-owned-book",
                    "kind": "create",
                    "display_name": "Foreign request",
                    "name": {"given": "Foreign"},
                })
                .to_string(),
            )
            .unwrap();
        fixture.deliver_owner_to_browser();

        let raw: Value = serde_json::from_str(
            &fixture
                .browser
                .client()
                .unwrap()
                .list_contact_requests_json(
                    &json!({"schema_version": 1, "book_id": "browser-owned-book"}).to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(raw["requests"].as_array().unwrap().len(), 1);
        assert_eq!(raw["requests"][0]["request_id"], "foreign-request");
        assert_eq!(
            fixture.command(
                "list_contact_edits",
                json!({"bookId": "browser-owned-book"}),
            ),
            json!([]),
            "the browser adapter must query by its own requester identity"
        );
    }

    #[test]
    fn photo_set_normalizes_through_core_and_removes_staged_plaintext_on_every_path() {
        let mut fixture = ContactFixture::new();
        let truncated_png = "data:image/png;base64,iVBORw0KGgo=";
        assert_eq!(
            fixture.error(
                "submit_contact_edit",
                json!({
                    "targetBookId": BOOK_ID,
                    "kind": "create",
                    "patches": [{"field": "givenName", "value": "Broken"}],
                    "photo": {"kind": "set", "croppedDataUrl": truncated_png},
                }),
            )["code"],
            "core"
        );
        assert_staging_empty(&fixture);

        let one_pixel_png = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNk+A8AAQUBAScY42YAAAAASUVORK5CYII=";
        let submitted = fixture.command(
            "submit_contact_edit",
            json!({
                "targetBookId": BOOK_ID,
                "kind": "create",
                "patches": [{"field": "givenName", "value": "Photo"}],
                "photo": {"kind": "set", "croppedDataUrl": one_pixel_png},
            }),
        );
        assert_eq!(
            object_keys(&submitted),
            BTreeSet::from(["requestId", "state"])
        );
        assert_eq!(submitted["state"], "pending");
        assert_staging_empty(&fixture);
        assert!(!submitted.to_string().contains("attachment"));
        assert!(!submitted.to_string().contains("file_key"));

        assert_eq!(
            fixture.error(
                "submit_contact_edit",
                json!({
                    "targetBookId": BOOK_ID,
                    "kind": "create",
                    "patches": [{"field": "givenName", "value": "Invalid"}],
                    "photo": {"kind": "set", "croppedDataUrl": "not-a-data-url"},
                }),
            )["code"],
            "contact-photo-invalid"
        );
        assert_staging_empty(&fixture);
    }

    #[test]
    fn deletion_restore_search_and_repair_are_real_sanitized_core_operations() {
        let mut fixture = ContactFixture::new();
        let contacts = fixture.command(
            "list_contacts",
            json!({"bookId": BOOK_ID, "query": "grace"}),
        );
        let submitted = fixture.command(
            "submit_contact_edit",
            json!({
                "targetBookId": BOOK_ID,
                "kind": "delete",
                "contactId": "c2",
                "baseRevision": contacts[0]["revision"],
            }),
        );
        let request_id = submitted["requestId"].as_str().unwrap().to_owned();
        fixture.deliver_browser_to_owner();
        let permit: Value = serde_json::from_str(
            &fixture
                .owner
                .next_contact_apply_permit(
                    &json!({"schema_version": 1, "request_id": request_id}).to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(permit["status"], "awaiting_approval");
        fixture
            .owner
            .contact_approval_json(
                &json!({"schema_version": 1, "request_id": request_id, "approve": true})
                    .to_string(),
            )
            .unwrap();
        let permit: Value = serde_json::from_str(
            &fixture
                .owner
                .next_contact_apply_permit(
                    &json!({"schema_version": 1, "request_id": request_id}).to_string(),
                )
                .unwrap(),
        )
        .unwrap();
        assert_eq!(permit["status"], "permit");
        fixture
            .owner
            .reconcile_contact_apply(
                &json!({
                    "schema_version": 1,
                    "request_id": request_id,
                    "outcome": "applied",
                })
                .to_string(),
            )
            .unwrap();
        fixture.deliver_owner_to_browser();

        let restorable = fixture.command("list_restorable_contacts", json!({"bookId": BOOK_ID}));
        assert_eq!(restorable.as_array().unwrap().len(), 1);
        assert_eq!(restorable[0]["id"], "c2");
        assert_eq!(restorable[0]["bookId"], BOOK_ID);
        assert_eq!(restorable[0]["displayName"], "Grace Lovelace");
        assert_eq!(
            object_keys(&restorable[0]),
            BTreeSet::from(["bookId", "deletedAt", "displayName", "id"])
        );
        let restored = fixture.command(
            "restore_contact",
            json!({"bookId": BOOK_ID, "contactId": "c2"}),
        );
        assert_eq!(restored["state"], "pending");
        assert!(restored["requestId"].as_str().is_some());

        let repair = fixture.command("request_contact_repair", json!({}));
        assert_eq!(repair["repairRequired"], true);
        assert!(repair.get("readiness").is_some());
        assert_eq!(
            fixture.command("contact_repair_status", json!({}))["repairRequired"],
            true
        );
        assert!(!repair.to_string().contains("native:secret"));

        assert_eq!(
            fixture.error("list_restorable_contacts", json!({"bookId": ""}),)["code"],
            "invalid-contact-edit"
        );
        assert_eq!(
            fixture.error("restore_contact", json!({"bookId": BOOK_ID}))["code"],
            "invalid-contact-edit"
        );
    }
}
