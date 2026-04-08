//! Router and `AppState` construction, extracted from `main.rs` for testability.

// Startup helpers promoted from `src/main.rs` (Story 6.6 AC9 Part A).
//
// These items lived as private functions in the binary crate and could not be
// called from integration tests in `tests/*.rs`. Promoting them here lets the
// test harness exercise the full startup path without duplicating the loader
// logic.
//
// IMPORTANT: This is a MECHANICAL, byte-exact move. No behaviour was changed.
// The only differences vs the original `src/main.rs` definitions are:
//   - `pub` added to `struct StartupProgram` and all four fields
//   - `pub` added to `async fn query_registered_programs`
//   - `use` statements for `sqlx::PgPool` and `tracing::{error, info, warn}`
//     are brought in explicitly (they were ambient in `main.rs`).

use sqlx::PgPool;
use tracing::{error, info, warn};

use crate::storage::StorageError;

/// Registered program info loaded from DB at startup.
pub struct StartupProgram {
    pub program_id: String,
    pub schema_name: String,
    pub idl: anchor_lang_idl_spec::Idl,
    /// Raw IDL JSON bytes as stored in `programs.idl_json`. Carried through
    /// to the cache seeding step so the in-memory cache holds the same bytes
    /// the hash was computed from. Story 4.4 AC5.
    pub idl_json: String,
}

/// Query the programs table for programs with persisted IDL JSON.
///
/// Returns programs with `status = 'schema_created'` and a non-null `idl_json` column,
/// parsing the IDL JSON into the `Idl` type for pipeline use.
///
/// A DB error is propagated as `StorageError::QueryFailed` so the supervisor
/// sees a non-zero exit instead of a silent "no programs" startup.
pub async fn query_registered_programs(pool: &PgPool) -> Result<Vec<StartupProgram>, StorageError> {
    let rows = sqlx::query_as::<_, (String, String, Option<String>)>(
        r#"SELECT "program_id", "schema_name", "idl_json" FROM "programs"
           WHERE "status" = 'schema_created'
           ORDER BY "program_id" ASC"#,
    )
    .fetch_all(pool)
    .await
    .map_err(|e| {
        error!(error = %e, "failed to query programs table");
        StorageError::QueryFailed(format!("programs lookup failed: {e}"))
    })?;

    let row_count = rows.len();
    if row_count == 0 {
        return Ok(Vec::new());
    }

    info!(count = row_count, "found registered program rows in DB");

    let mut programs = Vec::new();
    for (program_id, schema_name, idl_json) in rows {
        let Some(json) = idl_json else {
            warn!(program_id = %program_id, "program has no persisted idl_json, skipping pipeline auto-start");
            continue;
        };
        match serde_json::from_str::<anchor_lang_idl_spec::Idl>(&json) {
            Ok(idl) => {
                info!(program_id = %program_id, schema_name = %schema_name, "loaded persisted IDL");
                programs.push(StartupProgram {
                    program_id,
                    schema_name,
                    idl,
                    idl_json: json,
                });
            }
            Err(e) => {
                warn!(program_id = %program_id, error = %e, "failed to parse persisted IDL JSON");
            }
        }
    }

    if programs.is_empty() && row_count > 0 {
        error!(
            row_count,
            "all registered programs failed to load IDL JSON; pipeline will not auto-start"
        );
    } else {
        info!(
            loaded = programs.len(),
            row_count, "loaded persisted IDLs for pipeline auto-start"
        );
    }

    Ok(programs)
}
