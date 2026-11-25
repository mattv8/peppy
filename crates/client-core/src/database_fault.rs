//! The last SQLite failure converted on this thread, not necessarily the cause of a later
//! database error. Only coarse codes and source locations are recorded, never SQL or user data.

use std::cell::Cell;
use std::panic::Location;

#[repr(u16)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DatabaseFaultKind {
    SqliteFailure = 1,
    NoRows = 2,
    Decode = 3,
    Other = 4,
}

impl DatabaseFaultKind {
    pub const ALL: [Self; 4] = [Self::SqliteFailure, Self::NoRows, Self::Decode, Self::Other];

    pub const fn name(self) -> &'static str {
        match self {
            Self::SqliteFailure => "sqlite_failure",
            Self::NoRows => "no_rows",
            Self::Decode => "decode",
            Self::Other => "other",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DatabaseFault {
    pub kind: DatabaseFaultKind,
    pub sqlite_extended_code: Option<i32>,
    pub site: u32,
    pub line: u32,
    pub recorded_at_ms: i64,
}

/// FNV-1a of a source path after `src/`, masked to 31 bits.
pub fn database_fault_site(path_after_src: &str) -> u32 {
    path_after_src.bytes().fold(0x811c9dc5_u32, |hash, byte| {
        (hash ^ u32::from(byte)).wrapping_mul(0x01000193)
    }) & 0x7fff_ffff
}

fn source_site(path: &str) -> u32 {
    let normalized = path.replace('\\', "/");
    database_fault_site(
        normalized
            .rsplit_once("src/")
            .map_or(&normalized, |(_, suffix)| suffix),
    )
}

thread_local! {
    static LAST_FAULT: Cell<Option<DatabaseFault>> = const { Cell::new(None) };
}

pub(crate) fn note(error: &rusqlite::Error, location: &Location<'_>) {
    use rusqlite::Error;
    let (kind, sqlite_extended_code) = match error {
        Error::SqliteFailure(error, _) => {
            (DatabaseFaultKind::SqliteFailure, Some(error.extended_code))
        }
        Error::QueryReturnedNoRows => (DatabaseFaultKind::NoRows, None),
        Error::FromSqlConversionFailure(..)
        | Error::ToSqlConversionFailure(..)
        | Error::IntegralValueOutOfRange(..)
        | Error::Utf8Error(..)
        | Error::NulError(..)
        | Error::InvalidColumnType(..) => (DatabaseFaultKind::Decode, None),
        _ => (DatabaseFaultKind::Other, None),
    };
    LAST_FAULT.set(Some(DatabaseFault {
        kind,
        sqlite_extended_code,
        site: source_site(location.file()),
        line: location.line(),
        recorded_at_ms: crate::notifications::now_ms(),
    }));
}

/// Takes the last fault converted on the calling thread.
pub fn take_database_fault() -> Option<DatabaseFault> {
    LAST_FAULT.take()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[rustfmt::skip]
    fn convert_no_rows(line: &mut u32) -> Result<(), crate::Error> {
        { *line = line!(); Err::<(), _>(rusqlite::Error::QueryReturnedNoRows)? };
        Ok(())
    }

    #[test]
    fn conversion_records_caller_and_take_clears() {
        take_database_fault();
        let mut line = 0;
        assert_eq!(convert_no_rows(&mut line), Err(crate::Error::Database));
        let fault = take_database_fault().unwrap();
        assert_eq!(fault.kind, DatabaseFaultKind::NoRows);
        assert_eq!(fault.sqlite_extended_code, None);
        assert_eq!(fault.site, database_fault_site("database_fault.rs"));
        assert_eq!(fault.line, line);
        assert_eq!(take_database_fault(), None);
    }

    #[test]
    fn sqlite_failure_records_extended_code() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        conn.execute("CREATE TABLE test (value INTEGER UNIQUE)", [])
            .unwrap();
        conn.execute("INSERT INTO test VALUES (1)", []).unwrap();
        let error = conn.execute("INSERT INTO test VALUES (1)", []).unwrap_err();
        let expected = match &error {
            rusqlite::Error::SqliteFailure(error, _) => error.extended_code,
            _ => panic!("expected SQLite failure"),
        };
        let _: crate::Error = error.into();
        let fault = take_database_fault().unwrap();
        assert_eq!(fault.kind, DatabaseFaultKind::SqliteFailure);
        assert_eq!(fault.sqlite_extended_code, Some(expected));
    }

    #[test]
    fn decode_and_other_are_classified_without_content() {
        let _: crate::Error = rusqlite::Error::InvalidColumnType(
            0,
            "private".to_owned(),
            rusqlite::types::Type::Text,
        )
        .into();
        assert_eq!(
            take_database_fault().unwrap().kind,
            DatabaseFaultKind::Decode
        );
        let _: crate::Error = rusqlite::Error::InvalidQuery.into();
        assert_eq!(
            take_database_fault().unwrap().kind,
            DatabaseFaultKind::Other
        );
    }

    #[test]
    fn faults_are_thread_local() {
        take_database_fault();
        std::thread::spawn(|| {
            let _: crate::Error = rusqlite::Error::QueryReturnedNoRows.into();
        })
        .join()
        .unwrap();
        assert_eq!(take_database_fault(), None);

        let _: crate::Error = rusqlite::Error::InvalidQuery.into();
        let seen_elsewhere = std::thread::spawn(take_database_fault).join().unwrap();
        assert_eq!(seen_elsewhere, None);
        assert_eq!(
            take_database_fault().unwrap().kind,
            DatabaseFaultKind::Other
        );
    }

    #[test]
    fn source_paths_and_kind_table_are_stable() {
        let expected = database_fault_site("nested/file.rs");
        for path in [
            "/absolute/project/src/nested/file.rs",
            r"C:\project\src\nested\file.rs",
            "project/src/nested/file.rs",
        ] {
            assert_eq!(source_site(path), expected);
        }
        assert_eq!(
            DatabaseFaultKind::ALL.map(|kind| (kind as u16, kind.name())),
            [
                (1, "sqlite_failure"),
                (2, "no_rows"),
                (3, "decode"),
                (4, "other")
            ]
        );
    }
}
