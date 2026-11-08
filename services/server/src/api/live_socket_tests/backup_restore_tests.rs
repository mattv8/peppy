use super::*;

async fn snapshot_live_ticket_rows(server: &LiveTestServer) {
    for table in [
        "live_ticket_admission_events",
        "live_tickets",
        "live_ticket_sessions",
        "live_ticket_device_authority",
    ] {
        sqlx::query(&format!(
            "CREATE TABLE backup_{table} AS SELECT * FROM {table} WHERE vault_id=$1 AND device_id=$2"
        ))
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    }
}

async fn restore_live_ticket_rows(server: &LiveTestServer) {
    for table in [
        "live_tickets",
        "live_ticket_sessions",
        "live_ticket_device_authority",
        "live_ticket_admission_events",
    ] {
        sqlx::query(&format!(
            "DELETE FROM {table} WHERE vault_id=$1 AND device_id=$2"
        ))
        .bind(server.vault)
        .bind(server.owner_device)
        .execute(&server.pool)
        .await
        .unwrap();
    }
    for table in [
        "live_ticket_device_authority",
        "live_ticket_sessions",
        "live_tickets",
        "live_ticket_admission_events",
    ] {
        sqlx::query(&format!("INSERT INTO {table} SELECT * FROM backup_{table}"))
            .execute(&server.pool)
            .await
            .unwrap();
    }
}

#[tokio::test]
async fn raw_live_ticket_table_restore_revives_a_consumed_ticket_and_reuses_authority() {
    let server = LiveTestServer::start(FakeLiveBackend::immediate(), true).await;
    let session = lifecycle_session(220);
    let (first_ticket, handle) = issue_ticket_with_handle(&server, session.clone()).await;
    let (session_id, first_secret) = ticket_parts(&session, &first_ticket);
    snapshot_live_ticket_rows(&server).await;

    let first = consume_live_ticket(&server.pool, session_id, first_secret)
        .await
        .unwrap();
    let successor_ticket = reconnect_ticket(&server, &session, &handle).await;
    let (_, successor_secret) = ticket_parts(&session, &successor_ticket);
    let successor = consume_live_ticket(&server.pool, session_id, successor_secret)
        .await
        .unwrap();
    assert_ne!(successor.connection_id, first.connection_id);

    restore_live_ticket_rows(&server).await;

    let (mut revived_socket, _) = server.connect_with_ticket(&session, &first_ticket).await;
    expect_strict_hello(&mut revived_socket).await;
    let revived_connection: i64 = sqlx::query_scalar(
        "SELECT active_connection_id FROM live_ticket_sessions WHERE vault_id=$1 AND device_id=$2 AND session_id_hash=$3",
    )
    .bind(server.vault)
    .bind(server.owner_device)
    .bind(Sha256::digest(session_id).to_vec())
    .fetch_one(&server.pool)
    .await
    .unwrap();
    assert_eq!(revived_connection, first.connection_id);
    server.shutdown().await;
}
