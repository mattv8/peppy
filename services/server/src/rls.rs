//! RLS role validation and narrowly scoped privilege setup.
//!
//! These helpers are deliberately called only by the migration/startup paths.
//! Request handlers must select a vault before using tenant tables.

use sqlx::PgPool;
use thiserror::Error;

pub const TENANT_TABLES: &[&str] = &[
    "vaults",
    "devices",
    "device_credentials",
    "event_log",
    "encrypted_records",
    "commands",
    "command_receipts",
    "device_cursors",
    "outbox_jobs",
    "device_capabilities",
    "device_presence",
    "device_sockets",
    "upload_reservations",
    "attachments",
    "public_attachment_copies",
    "vault_key_profiles",
    "compacted_records",
    "attachment_record_references",
    "record_compaction",
    "record_supersessions",
    "storage_deletions",
    "device_wake_routes",
    "device_wake_jobs",
    "vault_admission_keys",
    "device_admissions",
    "phone_invitations",
    "computer_enrollments",
    "attachment_retention_uses",
    "attachment_retention_devices",
    "attachment_removals",
    "live_ticket_sessions",
    "live_tickets",
    "live_ticket_device_authority",
];

const MEMBERSHIP_VERSION_TABLE: &str = "membership_versions";

const CONTROL_PLANE_TABLES: &[&str] = &[
    "vault_owners",
    "instance_sessions",
    "vault_invites",
    "vault_requests",
];
const ADMISSION_TABLES: &[&str] = &["instance_admission_windows", "instance_admission_sources"];
const LIVE_TICKET_TABLES: &[&str] = &[
    "live_ticket_sessions",
    "live_tickets",
    "live_ticket_device_authority",
];
const LIVE_TICKET_RESOLVER: &str = "community_resolve_live_ticket_vault(bytea)";
const TENANT_SCOPE_POLICY_EXPRESSION: &str =
    "((vault_id)::text = NULLIF(current_setting('peppy.vault_id'::text, true), ''::text))";
const COMMUNITY_RESOLVERS: &[&str] = &[
    "community_resolve_device_credential(bytea)",
    "community_resolve_public_copy_vault(bytea)",
    "community_resolve_phone_invitation(bytea)",
    "community_resolve_phone_invitation_claim(uuid,bytea)",
    "community_resolve_computer_enrollment(uuid,bytea)",
    "community_resolve_device_admission(uuid)",
    "community_resolve_live_ticket_vault(bytea)",
    "community_admin_vault_summaries()",
];

#[derive(Debug, Error)]
pub enum RlsError {
    #[error("database role identifier is invalid")]
    InvalidRoleIdentifier,
    #[error("runtime database role is unsafe: {0}")]
    UnsafeRuntimeRole(&'static str),
    #[error("maintenance database role is unsafe: {0}")]
    UnsafeMaintenanceRole(&'static str),
    #[error(transparent)]
    Database(#[from] sqlx::Error),
}

pub fn role_identifier(role: &str) -> Result<&str, RlsError> {
    if role.is_empty()
        || role.len() > 63
        || !role.bytes().enumerate().all(|(index, byte)| {
            byte == b'_' || byte.is_ascii_alphanumeric() && (index != 0 || !byte.is_ascii_digit())
        })
    {
        return Err(RlsError::InvalidRoleIdentifier);
    }
    Ok(role)
}

fn quoted_role(role: &str) -> Result<String, RlsError> {
    Ok(format!("\"{}\"", role_identifier(role)?))
}

fn tenant_table_names() -> Vec<String> {
    TENANT_TABLES
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

async fn current_schema(pool: &PgPool) -> Result<String, RlsError> {
    sqlx::query_scalar("SELECT current_schema()")
        .fetch_one(pool)
        .await
        .map_err(RlsError::from)
}

fn quoted_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

fn qualified_tables(schema: &str, tables: &[&str]) -> String {
    let schema = quoted_identifier(schema);
    tables
        .iter()
        .map(|table| format!("{schema}.{}", quoted_identifier(table)))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Grants only scoped tenant DML and the two pre-scope resolver capabilities.
/// Invoke with the migration owner after migration 0021; never from a request.
pub async fn grant_runtime_privileges(pool: &PgPool, role: &str) -> Result<(), RlsError> {
    let role = quoted_role(role)?;
    let schema = current_schema(pool).await?;
    let schema_identifier = quoted_identifier(&schema);
    let tenant_tables = qualified_tables(&schema, TENANT_TABLES);
    let admission_tables = qualified_tables(&schema, ADMISSION_TABLES);
    let control_plane_tables = qualified_tables(&schema, CONTROL_PLANE_TABLES);
    let membership_version_table = qualified_tables(&schema, &[MEMBERSHIP_VERSION_TABLE]);
    sqlx::raw_sql(&format!(
        "GRANT USAGE ON SCHEMA {schema_identifier} TO {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE {tenant_tables} TO {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "GRANT SELECT ON TABLE {membership_version_table} TO {role}; REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER ON TABLE {membership_version_table} FROM {role}"
    ))
    .execute(pool)
    .await?;
    // Admission counters are control-plane rate-limit state, not vault rows.
    // They remain deliberately outside tenant RLS but need explicit runtime DML.
    sqlx::raw_sql(&format!(
        "GRANT SELECT, INSERT, UPDATE ON TABLE {admission_tables} TO {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "REVOKE DELETE, TRUNCATE, REFERENCES, TRIGGER ON TABLE {admission_tables} FROM {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "REVOKE ALL ON TABLE {schema_identifier}.peppy_schema_marker, {schema_identifier}._sqlx_migrations FROM {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "GRANT SELECT ON TABLE {schema_identifier}.peppy_schema_marker TO {role}"
    ))
    .execute(pool)
    .await?;
    let resolver_functions = COMMUNITY_RESOLVERS
        .iter()
        .map(|signature| format!("{schema_identifier}.{signature}"))
        .collect::<Vec<_>>()
        .join(", ");
    sqlx::raw_sql(&format!(
        "GRANT EXECUTE ON FUNCTION {resolver_functions} TO {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "GRANT SELECT, INSERT, UPDATE ON TABLE {control_plane_tables} TO {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "REVOKE DELETE, TRUNCATE, REFERENCES, TRIGGER ON TABLE {control_plane_tables} FROM {role}"
    ))
    .execute(pool)
    .await?;
    Ok(())
}

/// Grants cross-vault tenant maintenance without making the HTTP role privileged.
pub async fn grant_maintenance_privileges(pool: &PgPool, role: &str) -> Result<(), RlsError> {
    let role = quoted_role(role)?;
    let schema = current_schema(pool).await?;
    let schema_identifier = quoted_identifier(&schema);
    let tenant_tables = qualified_tables(&schema, TENANT_TABLES);
    let admission_tables = qualified_tables(&schema, ADMISSION_TABLES);
    let control_plane_tables = qualified_tables(&schema, CONTROL_PLANE_TABLES);
    let membership_version_table = qualified_tables(&schema, &[MEMBERSHIP_VERSION_TABLE]);
    sqlx::raw_sql(&format!(
        "GRANT USAGE ON SCHEMA {schema_identifier} TO {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "GRANT SELECT ON TABLE {membership_version_table} TO {role}; REVOKE INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER ON TABLE {membership_version_table} FROM {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "GRANT SELECT, INSERT, UPDATE, DELETE ON TABLE {tenant_tables} TO {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "GRANT SELECT, DELETE ON TABLE {control_plane_tables} TO {role}"
    ))
    .execute(pool)
    .await?;
    sqlx::raw_sql(&format!(
        "GRANT SELECT, UPDATE, DELETE ON TABLE {admission_tables} TO {role}"
    ))
    .execute(pool)
    .await?;
    Ok(())
}

/// Validates a non-owner runtime role. `community` selects the public
/// community resolver contract; hosted callers retain their private resolver.
pub async fn validate_runtime_role(pool: &PgPool, community: bool) -> Result<(), RlsError> {
    let (superuser, bypass_rls, create_role): (bool, bool, bool) = sqlx::query_as(
        "SELECT rolsuper, rolbypassrls, rolcreaterole FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(pool)
    .await?;
    if superuser {
        return Err(RlsError::UnsafeRuntimeRole("superuser"));
    }
    if bypass_rls {
        return Err(RlsError::UnsafeRuntimeRole("BYPASSRLS"));
    }
    if create_role {
        return Err(RlsError::UnsafeRuntimeRole("CREATEROLE"));
    }
    let owns_table: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relkind IN ('r', 'p', 'v', 'm', 'S', 'f') AND relowner = (SELECT oid FROM pg_roles WHERE rolname = current_user))",
    )
    .fetch_one(pool)
    .await?;
    if owns_table {
        return Err(RlsError::UnsafeRuntimeRole("table owner"));
    }
    let has_role_membership: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_roles role WHERE role.rolname <> current_user AND pg_has_role(current_user, role.oid, 'MEMBER'))",
    )
    .fetch_one(pool)
    .await?;
    if has_role_membership {
        return Err(RlsError::UnsafeRuntimeRole("role membership"));
    }
    validate_rls_schema(pool, community).await?;
    validate_live_ticket_runtime_capabilities(pool).await?;
    validate_membership_version_runtime_privileges(pool).await?;
    validate_admission_runtime_privileges(pool).await?;
    if community {
        validate_community_privileges(pool).await?;
    }
    Ok(())
}

async fn validate_membership_version_runtime_privileges(pool: &PgPool) -> Result<(), RlsError> {
    let safe: bool = sqlx::query_scalar(
        "SELECT relrowsecurity AND NOT relforcerowsecurity AND has_table_privilege(current_user, oid, 'SELECT') AND NOT has_table_privilege(current_user, oid, 'INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER') FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = $1",
    )
    .bind(MEMBERSHIP_VERSION_TABLE)
    .fetch_optional(pool)
    .await?
    .unwrap_or(false);
    if !safe {
        return Err(RlsError::UnsafeRuntimeRole(
            "membership version grant is missing or unsafe",
        ));
    }
    Ok(())
}

async fn validate_rls_schema(pool: &PgPool, community: bool) -> Result<(), RlsError> {
    let (table_count, all_enabled, no_force): (i64, bool, bool) = sqlx::query_as(
        "SELECT count(*), COALESCE(bool_and(relrowsecurity), false), COALESCE(bool_and(NOT relforcerowsecurity), false) FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = ANY($1)",
    )
    .bind(tenant_table_names())
    .fetch_one(pool)
    .await?;
    if table_count != TENANT_TABLES.len() as i64 || !all_enabled {
        return Err(RlsError::UnsafeRuntimeRole(
            "RLS is not enabled on every tenant table",
        ));
    }
    if !no_force {
        return Err(RlsError::UnsafeRuntimeRole(
            "FORCE ROW LEVEL SECURITY is enabled",
        ));
    }
    let (policy_count, policy_table_count, policies_are_safe): (i64, i64, bool) =
        sqlx::query_as(
            "SELECT count(*), count(DISTINCT relation.relname), COALESCE(bool_and(policy.polpermissive AND policy.polcmd = '*' AND policy.polroles = ARRAY[0::oid] AND COALESCE(btrim(regexp_replace(pg_get_expr(policy.polqual, policy.polrelid), '[[:space:]]+', ' ', 'g')) = $2, false) AND COALESCE(btrim(regexp_replace(pg_get_expr(policy.polwithcheck, policy.polrelid), '[[:space:]]+', ' ', 'g')) = $2, false) AND ($3 = false OR policy.polname = 'community_' || relation.relname || '_scope')), false) FROM pg_policy policy JOIN pg_class relation ON relation.oid = policy.polrelid WHERE relation.relnamespace = current_schema()::regnamespace AND relation.relname = ANY($1)",
        )
        .bind(tenant_table_names())
        .bind(TENANT_SCOPE_POLICY_EXPRESSION)
        .bind(community)
        .fetch_one(pool)
        .await?;
    if policy_table_count != TENANT_TABLES.len() as i64 {
        return Err(RlsError::UnsafeRuntimeRole("tenant policy is missing"));
    }
    if !policies_are_safe || community && policy_count != TENANT_TABLES.len() as i64 {
        return Err(RlsError::UnsafeRuntimeRole("tenant policy is unsafe"));
    }
    if !community {
        return Ok(());
    }
    for resolver in COMMUNITY_RESOLVERS {
        let exists: bool = sqlx::query_scalar("SELECT to_regprocedure($1) IS NOT NULL")
            .bind(resolver)
            .fetch_one(pool)
            .await?;
        if !exists {
            return Err(RlsError::UnsafeRuntimeRole("community resolver is missing"));
        }
        let (runtime_execute, public_execute, runtime_owns): (bool, bool, bool) = sqlx::query_as(
            "SELECT has_function_privilege(current_user, $1, 'EXECUTE'), has_function_privilege('public', $1, 'EXECUTE'), p.proowner = (SELECT oid FROM pg_roles WHERE rolname=current_user) FROM pg_proc p WHERE p.oid=to_regprocedure($1)",
        )
        .bind(resolver)
        .fetch_one(pool)
        .await?;
        if !runtime_execute || public_execute {
            return Err(RlsError::UnsafeRuntimeRole(
                "community resolver execute grant is missing",
            ));
        }
        if runtime_owns {
            return Err(RlsError::UnsafeRuntimeRole("community resolver owner"));
        }
    }
    let can_create_in_search_path: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM unnest(current_schemas(true)) schema_name WHERE schema_name NOT IN ('pg_catalog', 'pg_temp') AND has_schema_privilege(current_user, schema_name, 'CREATE'))",
    )
    .fetch_one(pool)
    .await?;
    if can_create_in_search_path {
        return Err(RlsError::UnsafeRuntimeRole("CREATE on active schema"));
    }
    Ok(())
}

/// Validates the ticket pre-scope resolver and scoped table privileges required
/// by both community and hosted runtime assemblies.
pub async fn validate_live_ticket_runtime_capabilities(pool: &PgPool) -> Result<(), RlsError> {
    let (table_count, all_enabled, no_force, scoped_dml): (i64, bool, bool, bool) = sqlx::query_as(
        "SELECT count(*), COALESCE(bool_and(relrowsecurity), false), COALESCE(bool_and(NOT relforcerowsecurity), false), COALESCE(bool_and(has_table_privilege(current_user, oid, 'SELECT') AND has_table_privilege(current_user, oid, 'INSERT') AND has_table_privilege(current_user, oid, 'UPDATE') AND has_table_privilege(current_user, oid, 'DELETE') AND NOT has_table_privilege(current_user, oid, 'TRUNCATE') AND NOT has_table_privilege(current_user, oid, 'REFERENCES') AND NOT has_table_privilege(current_user, oid, 'TRIGGER')), false) FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = ANY($1)",
    )
    .bind(LIVE_TICKET_TABLES)
    .fetch_one(pool)
    .await?;
    if table_count != LIVE_TICKET_TABLES.len() as i64 || !all_enabled || !no_force {
        return Err(RlsError::UnsafeRuntimeRole("live ticket RLS is unsafe"));
    }
    if !scoped_dml {
        return Err(RlsError::UnsafeRuntimeRole(
            "live ticket runtime DML grant is missing or unsafe",
        ));
    }
    let (security_definer, fixed_search_path, runtime_execute, public_execute, runtime_owns):
        (bool, bool, bool, bool, bool) = sqlx::query_as(
            "SELECT p.prosecdef, p.proconfig = ARRAY['search_path=pg_catalog, public, pg_temp'], has_function_privilege(current_user, $1, 'EXECUTE'), has_function_privilege('public', $1, 'EXECUTE'), p.proowner = (SELECT oid FROM pg_roles WHERE rolname=current_user) FROM pg_proc p WHERE p.oid=to_regprocedure($1)",
        )
        .bind(LIVE_TICKET_RESOLVER)
        .fetch_optional(pool)
        .await?
        .ok_or(RlsError::UnsafeRuntimeRole("live ticket resolver is missing"))?;
    if !security_definer || !fixed_search_path || runtime_owns {
        return Err(RlsError::UnsafeRuntimeRole(
            "live ticket resolver is unsafe",
        ));
    }
    if !runtime_execute || public_execute {
        return Err(RlsError::UnsafeRuntimeRole(
            "live ticket resolver execute grant is missing",
        ));
    }
    Ok(())
}

pub async fn validate_admission_runtime_privileges(pool: &PgPool) -> Result<(), RlsError> {
    let privileges_are_safe: bool = sqlx::query_scalar(
        "SELECT count(*) = $2 AND COALESCE(bool_and(has_table_privilege(current_user, oid, 'SELECT') AND has_table_privilege(current_user, oid, 'INSERT') AND has_table_privilege(current_user, oid, 'UPDATE') AND NOT has_table_privilege(current_user, oid, 'DELETE') AND NOT has_table_privilege(current_user, oid, 'TRUNCATE') AND NOT has_table_privilege(current_user, oid, 'REFERENCES') AND NOT has_table_privilege(current_user, oid, 'TRIGGER')), false) FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = ANY($1)",
    )
    .bind(ADMISSION_TABLES.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>())
    .bind(ADMISSION_TABLES.len() as i64)
    .fetch_one(pool)
    .await?;
    if !privileges_are_safe {
        return Err(RlsError::UnsafeRuntimeRole(
            "admission ledger grant is missing or unsafe",
        ));
    }
    Ok(())
}

pub async fn validate_admission_maintenance_privileges(pool: &PgPool) -> Result<(), RlsError> {
    let has_required_privileges: bool = sqlx::query_scalar(
        "SELECT count(*) = $2 AND COALESCE(bool_and(has_table_privilege(current_user, oid, 'SELECT') AND has_table_privilege(current_user, oid, 'UPDATE') AND has_table_privilege(current_user, oid, 'DELETE')), false) FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = ANY($1)",
    )
    .bind(ADMISSION_TABLES.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>())
    .bind(ADMISSION_TABLES.len() as i64)
    .fetch_one(pool)
    .await?;
    if !has_required_privileges {
        return Err(RlsError::UnsafeMaintenanceRole(
            "admission ledger grant is missing",
        ));
    }
    Ok(())
}

async fn validate_community_privileges(pool: &PgPool) -> Result<(), RlsError> {
    let tenant_dml: bool = sqlx::query_scalar(
        "SELECT count(*) = $2 AND COALESCE(bool_and(has_table_privilege(current_user, oid, 'SELECT') AND has_table_privilege(current_user, oid, 'INSERT') AND has_table_privilege(current_user, oid, 'UPDATE') AND has_table_privilege(current_user, oid, 'DELETE') AND NOT has_table_privilege(current_user, oid, 'TRUNCATE') AND NOT has_table_privilege(current_user, oid, 'REFERENCES') AND NOT has_table_privilege(current_user, oid, 'TRIGGER')), false) FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = ANY($1)",
    )
    .bind(tenant_table_names())
    .bind(TENANT_TABLES.len() as i64)
    .fetch_one(pool)
    .await?;
    if !tenant_dml {
        return Err(RlsError::UnsafeRuntimeRole("tenant DML grant is missing"));
    }
    let control_plane: bool = sqlx::query_scalar(
        "SELECT count(*) = $2 AND COALESCE(bool_and(NOT relrowsecurity AND has_table_privilege(current_user, oid, 'SELECT') AND has_table_privilege(current_user, oid, 'INSERT') AND has_table_privilege(current_user, oid, 'UPDATE') AND NOT has_table_privilege(current_user, oid, 'DELETE, TRUNCATE, REFERENCES, TRIGGER')), false) FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = ANY($1)",
    )
    .bind(CONTROL_PLANE_TABLES.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>())
    .bind(CONTROL_PLANE_TABLES.len() as i64)
    .fetch_one(pool)
    .await?;
    if !control_plane {
        return Err(RlsError::UnsafeRuntimeRole(
            "control-plane grant or RLS state is unsafe",
        ));
    }
    let schema_metadata: bool = sqlx::query_scalar(
        "SELECT has_table_privilege(current_user, 'peppy_schema_marker', 'SELECT') AND NOT (has_table_privilege(current_user, 'peppy_schema_marker', 'INSERT') OR has_table_privilege(current_user, 'peppy_schema_marker', 'UPDATE') OR has_table_privilege(current_user, 'peppy_schema_marker', 'DELETE') OR has_table_privilege(current_user, 'peppy_schema_marker', 'TRUNCATE') OR has_table_privilege(current_user, 'peppy_schema_marker', 'REFERENCES') OR has_table_privilege(current_user, 'peppy_schema_marker', 'TRIGGER')) AND NOT (has_table_privilege(current_user, '_sqlx_migrations', 'SELECT') OR has_table_privilege(current_user, '_sqlx_migrations', 'INSERT') OR has_table_privilege(current_user, '_sqlx_migrations', 'UPDATE') OR has_table_privilege(current_user, '_sqlx_migrations', 'DELETE') OR has_table_privilege(current_user, '_sqlx_migrations', 'TRUNCATE') OR has_table_privilege(current_user, '_sqlx_migrations', 'REFERENCES') OR has_table_privilege(current_user, '_sqlx_migrations', 'TRIGGER'))",
    )
    .fetch_one(pool)
    .await?;
    if !schema_metadata {
        return Err(RlsError::UnsafeRuntimeRole(
            "schema metadata grant is unsafe",
        ));
    }
    let mut allowed = tenant_table_names();
    allowed.extend(CONTROL_PLANE_TABLES.iter().map(|name| (*name).to_owned()));
    allowed.extend([
        MEMBERSHIP_VERSION_TABLE.to_owned(),
        "instance_admission_windows".to_owned(),
        "instance_admission_sources".to_owned(),
        "_sqlx_migrations".to_owned(),
        "peppy_schema_marker".to_owned(),
    ]);
    let unexpected_grant: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relkind IN ('r', 'p', 'v', 'm', 'S', 'f') AND relname <> ALL($1) AND has_table_privilege(current_user, oid, 'SELECT, INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER'))",
    )
    .bind(allowed)
    .fetch_one(pool)
    .await?;
    if unexpected_grant {
        return Err(RlsError::UnsafeRuntimeRole(
            "unexpected public-table privilege",
        ));
    }
    Ok(())
}

pub async fn validate_maintenance_role(pool: &PgPool) -> Result<(), RlsError> {
    let (superuser, bypass_rls, create_role): (bool, bool, bool) = sqlx::query_as(
        "SELECT rolsuper, rolbypassrls, rolcreaterole FROM pg_roles WHERE rolname = current_user",
    )
    .fetch_one(pool)
    .await?;
    if superuser {
        return Err(RlsError::UnsafeMaintenanceRole("superuser"));
    }
    if !bypass_rls {
        return Err(RlsError::UnsafeMaintenanceRole("requires BYPASSRLS"));
    }
    if create_role {
        return Err(RlsError::UnsafeMaintenanceRole("CREATEROLE"));
    }
    let owns_table: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relkind IN ('r', 'p', 'v', 'm', 'S', 'f') AND relowner = (SELECT oid FROM pg_roles WHERE rolname = current_user))",
    )
    .fetch_one(pool)
    .await?;
    if owns_table {
        return Err(RlsError::UnsafeMaintenanceRole("table owner"));
    }
    let has_role_membership: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_roles role WHERE role.rolname <> current_user AND pg_has_role(current_user, role.oid, 'MEMBER'))",
    )
    .fetch_one(pool)
    .await?;
    if has_role_membership {
        return Err(RlsError::UnsafeMaintenanceRole("role membership"));
    }
    let tenant_dml: bool = sqlx::query_scalar(
        "SELECT count(*) = $2 AND COALESCE(bool_and(has_table_privilege(current_user, oid, 'SELECT') AND has_table_privilege(current_user, oid, 'INSERT') AND has_table_privilege(current_user, oid, 'UPDATE') AND has_table_privilege(current_user, oid, 'DELETE')), false) FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = ANY($1)",
    )
    .bind(tenant_table_names())
    .bind(TENANT_TABLES.len() as i64)
    .fetch_one(pool)
    .await?;
    if !tenant_dml {
        return Err(RlsError::UnsafeMaintenanceRole("requires tenant DML"));
    }
    validate_membership_version_maintenance_privileges(pool).await?;
    let control_plane_sweep: bool = sqlx::query_scalar(
        "SELECT count(*) = $2 AND COALESCE(bool_and(has_table_privilege(current_user, oid, 'SELECT') AND has_table_privilege(current_user, oid, 'DELETE')), false) FROM pg_class WHERE relnamespace = current_schema()::regnamespace AND relname = ANY($1)",
    )
    .bind(CONTROL_PLANE_TABLES.iter().map(|name| (*name).to_owned()).collect::<Vec<_>>())
    .bind(CONTROL_PLANE_TABLES.len() as i64)
    .fetch_one(pool)
    .await?;
    if !control_plane_sweep {
        return Err(RlsError::UnsafeMaintenanceRole(
            "requires global sweep privileges",
        ));
    }
    validate_admission_maintenance_privileges(pool).await
}

async fn validate_membership_version_maintenance_privileges(pool: &PgPool) -> Result<(), RlsError> {
    let safe: bool = sqlx::query_scalar(
        "SELECT has_table_privilege(current_user, 'membership_versions', 'SELECT') AND NOT has_table_privilege(current_user, 'membership_versions', 'INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER')",
    )
    .fetch_one(pool)
    .await?;
    if !safe {
        return Err(RlsError::UnsafeMaintenanceRole(
            "membership version grant is unsafe",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::role_identifier;

    #[test]
    fn role_identifiers_are_strictly_validated() {
        assert!(role_identifier("peppy_runtime_1").is_ok());
        for invalid in ["", "1runtime", "runtime;DROP ROLE x", "runtime-role"] {
            assert!(role_identifier(invalid).is_err());
        }
    }
}

#[cfg(test)]
mod live_ticket_schema_tests {
    use super::TENANT_TABLES;

    #[test]
    fn live_ticket_tables_are_part_of_the_tenant_rls_contract() {
        assert!(TENANT_TABLES.contains(&"live_ticket_sessions"));
        assert!(TENANT_TABLES.contains(&"live_tickets"));
        assert!(TENANT_TABLES.contains(&"live_ticket_device_authority"));
    }
}
