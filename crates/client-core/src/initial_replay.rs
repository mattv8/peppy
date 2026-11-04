use crate::{Error, get_meta, receive_cursor, set_meta};
use rusqlite::Connection;

const TARGET_META: &str = "initial_replay_target";
const ORIGIN_BACKFILL_COMPLETE_META: &str = "message_origin_backfill_complete";

pub(crate) fn observe_server_high_water(conn: &Connection, cursor: u64) -> Result<(), Error> {
    if get_meta(conn, TARGET_META)?.is_none() {
        set_meta(conn, TARGET_META, &cursor.to_string())?;
    }
    Ok(())
}

/// Advances the durable origin-index backfill before checking replay readiness.
pub(crate) fn caught_up(conn: &Connection) -> Result<bool, Error> {
    crate::message_origin::backfill_step(conn)?;
    let Some(target) = get_meta(conn, TARGET_META)? else {
        return Ok(false);
    };
    let target = target.parse::<u64>().map_err(|_| Error::Database)?;
    Ok(receive_cursor(conn)? >= target
        && get_meta(conn, ORIGIN_BACKFILL_COMPLETE_META)?.as_deref() == Some("1"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn() -> Connection {
        let conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE metadata(k TEXT PRIMARY KEY, v TEXT NOT NULL); CREATE TABLE messages(local_order INTEGER PRIMARY KEY, id TEXT UNIQUE NOT NULL, payload TEXT NOT NULL)")
            .unwrap();
        crate::message_origin::initialize(&conn).unwrap();
        conn
    }

    #[test]
    fn target_is_recorded_once() {
        let conn = conn();
        observe_server_high_water(&conn, 4).unwrap();
        observe_server_high_water(&conn, 9).unwrap();
        assert_eq!(get_meta(&conn, TARGET_META).unwrap().as_deref(), Some("4"));
    }

    #[test]
    fn caught_up_completes_empty_origin_backfill() {
        let conn = conn();
        assert!(!caught_up(&conn).unwrap());
        observe_server_high_water(&conn, 4).unwrap();
        set_meta(&conn, "receive_cursor", "4").unwrap();
        assert!(caught_up(&conn).unwrap());
    }
}
