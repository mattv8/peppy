use super::{LATEST_CONTEXTUAL_MMS_SQL, LATEST_MMS_SQL, install_query_indexes};
use crate::{
    Client, ClientConfig, ConversationId, DatabaseKey, DeviceId, Direction, MessageId,
    MessagePayload, MmsContext, SourceSequence, Transport, VaultId,
};
use peppy_domain::MessageRecord;
use rusqlite::{Connection, OptionalExtension, StatementStatus, params};

const SMS_ROWS: i64 = 4_000;
const MAX_INDEXED_VM_STEPS: i32 = 40;

fn query_steps(conn: &Connection, sql: &str, conversation: ConversationId) -> i32 {
    let mut statement = conn.prepare(sql).unwrap();
    let _: Option<String> = statement
        .query_row(params![conversation.to_string()], |row| row.get(0))
        .optional()
        .unwrap();
    statement.get_status(StatementStatus::VmStep)
}

fn query_plan(conn: &Connection, sql: &str, conversation: ConversationId) -> String {
    conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params![conversation.to_string()], |row| {
            row.get::<_, String>(3)
        })
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n")
}

fn insert_sms_history(conn: &mut Connection, conversation: ConversationId) {
    let transaction = conn.transaction().unwrap();
    {
        let mut insert = transaction.prepare(
            "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen,sort_at_ms) VALUES(?,?,?,?,?,0,?)",
        ).unwrap();
        let source = DeviceId::new().to_string();
        for sequence in 1..=SMS_ROWS {
            insert
                .execute(params![
                    format!("sms-{sequence}"),
                    conversation.to_string(),
                    source,
                    sequence,
                    r#"{"transport":"sms"}"#,
                    sequence,
                ])
                .unwrap();
        }
    }
    transaction.commit().unwrap();
}

#[test]
fn partial_indexes_bound_sms_only_reply_lookup_work() {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch(
        "CREATE TABLE messages(local_order INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,conversation_id TEXT NOT NULL,source_device_id TEXT NOT NULL,source_sequence INTEGER NOT NULL,payload TEXT NOT NULL,seen INTEGER NOT NULL,sort_at_ms INTEGER NOT NULL); CREATE INDEX messages_sort ON messages(conversation_id,sort_at_ms,local_order);",
    ).unwrap();
    let conversation = ConversationId::new();
    insert_sms_history(&mut conn, conversation);

    assert!(query_steps(&conn, LATEST_MMS_SQL, conversation) > SMS_ROWS as i32);
    assert!(query_steps(&conn, LATEST_CONTEXTUAL_MMS_SQL, conversation) > SMS_ROWS as i32);

    install_query_indexes(&conn).unwrap();

    let latest_steps = query_steps(&conn, LATEST_MMS_SQL, conversation);
    let contextual_steps = query_steps(&conn, LATEST_CONTEXTUAL_MMS_SQL, conversation);
    assert!(
        latest_steps <= MAX_INDEXED_VM_STEPS,
        "latest MMS used {latest_steps} VM steps"
    );
    assert!(
        contextual_steps <= MAX_INDEXED_VM_STEPS,
        "contextual MMS used {contextual_steps} VM steps"
    );
    assert!(query_plan(&conn, LATEST_MMS_SQL, conversation).contains("messages_latest_mms"));
    assert!(
        query_plan(&conn, LATEST_CONTEXTUAL_MMS_SQL, conversation)
            .contains("messages_latest_mms_context")
    );
}

fn insert_message(client: &Client, message: &MessagePayload) {
    let store = client.lock().unwrap();
    store.conn.execute(
        "INSERT INTO messages(id,conversation_id,source_device_id,source_sequence,payload,seen,sort_at_ms) VALUES(?,?,?,?,?,0,10)",
        params![
            message.record.message_id.to_string(), message.record.conversation_id.to_string(),
            message.source_device_id.to_string(), message.record.source_sequence.0,
            serde_json::to_string(message).unwrap(),
        ],
    ).unwrap();
}

#[test]
fn indexes_preserve_reply_metadata_and_source_sim_guard() {
    let directory = tempfile::TempDir::new().unwrap();
    let client = Client::open(
        ClientConfig {
            database_path: directory.path().join("reply.db"),
            vault_id: VaultId::new(),
            device_id: DeviceId::new(),
        },
        DatabaseKey::new(&[31; 32]).unwrap(),
    )
    .unwrap();
    {
        let store = client.lock().unwrap();
        store
            .conn
            .execute_batch(
                "DROP INDEX messages_latest_mms; DROP INDEX messages_latest_mms_context;",
            )
            .unwrap();
    }
    let conversation = ConversationId::new();
    let source = DeviceId::new();
    let other_source = DeviceId::new();
    let message = MessagePayload {
        record: MessageRecord {
            message_id: MessageId::new(),
            conversation_id: conversation,
            source_sequence: SourceSequence(1),
            attachments: vec![],
        },
        source_device_id: source,
        provider_message_id: Some("provider-1".into()),
        sender_address: Some("+15555550101".into()),
        recipients: vec!["+15555550100".into(), "+15555550102".into()],
        subject: Some("Group subject".into()),
        body: "Group body".into(),
        transport: Transport::Mms,
        direction: Direction::Incoming,
        imported: false,
        sent_at_ms: Some(10),
        ordered_at_ms: None,
        mms_context: Some(MmsContext {
            source_generation: "generation-1".into(),
            subscription_id: "sim-1".into(),
            provider_thread_id: Some("thread-1".into()),
        }),
    };
    insert_message(&client, &message);
    {
        let store = client.lock().unwrap();
        super::apply_mms_own_address(&store.conn, other_source, "sim-1", "+15555550100", 1)
            .unwrap();
    }
    let before = client.mms_reply_context(conversation).unwrap();
    assert_eq!(
        before.recipients,
        vec![
            "+15555550101".to_owned(),
            "+15555550100".to_owned(),
            "+15555550102".to_owned()
        ]
    );
    assert_eq!(before.subject.as_deref(), Some("Group subject"));

    {
        let store = client.lock().unwrap();
        install_query_indexes(&store.conn).unwrap();
    }
    let after = client.mms_reply_context(conversation).unwrap();
    assert_eq!(after, before);

    {
        let store = client.lock().unwrap();
        super::apply_mms_own_address(&store.conn, source, "sim-1", "+15555550100", 1).unwrap();
    }
    let confirmed_own = client.mms_reply_context(conversation).unwrap();
    assert_eq!(
        confirmed_own.recipients,
        vec!["+15555550101".to_owned(), "+15555550102".to_owned()]
    );
    assert_eq!(confirmed_own.subject.as_deref(), Some("Group subject"));
}
