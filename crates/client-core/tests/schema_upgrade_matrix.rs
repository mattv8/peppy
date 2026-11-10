//! Upgrade matrix: every supported historical SQLCipher layout must open at the current schema
//! version and converge to the same table, column, and index shape as a fresh database.
use crate::common::*;
use peppy_client_core::{Client, DatabaseKey, Error};
use rusqlite::Connection;
use std::collections::BTreeSet;
use tempfile::TempDir;

const CURRENT_SCHEMA_VERSION: i64 = 57;

fn fixture_path(name: &str) -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name)
}

fn config_at(directory: &TempDir, file: &str) -> ClientConfig {
    ClientConfig {
        database_path: directory.path().join(file),
        vault_id: VaultId(uuid::Uuid::parse_str("aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa").unwrap()),
        device_id: DeviceId(uuid::Uuid::parse_str("bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb").unwrap()),
    }
}

fn raw_connection(path: &std::path::Path) -> Connection {
    let connection = Connection::open(path).unwrap();
    let key: String = DB_KEY.iter().map(|byte| format!("{byte:02x}")).collect();
    connection
        .execute_batch(&format!(
            "PRAGMA key = \"x'{key}'\"; PRAGMA cipher_compatibility = 4;"
        ))
        .unwrap();
    connection
}

fn schema_version(connection: &Connection) -> i64 {
    connection
        .query_row("SELECT version FROM schema_meta", [], |row| row.get(0))
        .unwrap()
}

/// Flattens every user table, column, and index name into one comparable set.
fn schema_shape(connection: &Connection) -> BTreeSet<String> {
    let tables: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    let mut shape = BTreeSet::new();
    for table in tables {
        let columns: Vec<String> = connection
            .prepare(&format!("PRAGMA table_info({table})"))
            .unwrap()
            .query_map([], |row| row.get::<_, String>(1))
            .unwrap()
            .map(Result::unwrap)
            .collect();
        shape.insert(format!("table {table}"));
        for column in columns {
            shape.insert(format!("column {table}.{column}"));
        }
    }
    let indexes: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_master WHERE type='index' AND name NOT LIKE 'sqlite_%'")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    shape.extend(indexes.into_iter().map(|index| format!("index {index}")));
    shape
}

fn fresh_shape() -> BTreeSet<String> {
    let directory = TempDir::new().unwrap();
    let config = config_at(&directory, "fresh.db");
    drop(open(&config));
    let connection = raw_connection(&config.database_path);
    assert_eq!(schema_version(&connection), CURRENT_SCHEMA_VERSION);
    schema_shape(&connection)
}

/// Copies a historical fixture, proves its source version, opens it with the current code, and
/// returns the upgraded shape. The returned directory keeps the upgraded database alive.
fn upgrade_fixture(name: &str, source_version: i64) -> (TempDir, ClientConfig, BTreeSet<String>) {
    let directory = TempDir::new().unwrap();
    let config = config_at(&directory, "upgrade.db");
    std::fs::copy(fixture_path(name), &config.database_path).unwrap();
    {
        let source = raw_connection(&config.database_path);
        assert_eq!(schema_version(&source), source_version, "{name}");
    }
    drop(open(&config));
    let upgraded = raw_connection(&config.database_path);
    assert_eq!(schema_version(&upgraded), CURRENT_SCHEMA_VERSION, "{name}");
    let shape = schema_shape(&upgraded);
    (directory, config, shape)
}

#[test]
fn production_v50_layout_upgrades_to_current_and_converges_with_fresh() {
    let (_directory, _config, shape) =
        upgrade_fixture("client_schema_production_v50.sqlcipher-fixture", 50);
    assert_eq!(shape, fresh_shape());
}

#[test]
fn branch_v50_layout_upgrades_to_current_and_converges_with_fresh() {
    let (_directory, _config, shape) = upgrade_fixture("client_schema_v50.sqlcipher-fixture", 50);
    assert_eq!(shape, fresh_shape());
}

#[test]
fn branch_v51_layout_upgrades_to_current_and_converges_with_fresh() {
    let (_directory, _config, shape) =
        upgrade_fixture("client_schema_branch_v51.sqlcipher-fixture", 51);
    assert_eq!(shape, fresh_shape());
}

#[test]
fn base_v46_layout_upgrades_to_current_and_converges_with_fresh() {
    let (_directory, _config, shape) =
        upgrade_fixture("client_schema_base_v46.sqlcipher-fixture", 46);
    assert_eq!(shape, fresh_shape());
}

#[test]
fn upgraded_database_rejects_the_next_unknown_schema_version() {
    let (_directory, config, _shape) =
        upgrade_fixture("client_schema_branch_v51.sqlcipher-fixture", 51);
    {
        let connection = raw_connection(&config.database_path);
        connection
            .execute(
                "UPDATE schema_meta SET version=?",
                [CURRENT_SCHEMA_VERSION + 1],
            )
            .unwrap();
    }
    assert!(matches!(
        Client::open(config, DatabaseKey::new(&DB_KEY).unwrap()),
        Err(Error::UnsupportedSchema)
    ));
}
