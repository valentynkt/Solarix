pub mod fetch;

use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;
use std::time::Duration;

use anchor_lang_idl_spec::Idl;
use sha2::{Digest, Sha256};
use tracing::{debug, info, warn};

use crate::idl::fetch::{fetch_idl_from_bundled, fetch_idl_from_chain};

/// Source from which an IDL was acquired.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IdlSource {
    OnChain,
    Bundled,
    Manual,
}

impl IdlSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            IdlSource::OnChain => "onchain",
            IdlSource::Bundled => "bundled",
            IdlSource::Manual => "manual",
        }
    }
}

/// Cached IDL entry with metadata.
///
/// `raw_json` holds the **original fetched/uploaded JSON bytes** — not a
/// re-serialization of `idl`. This is what `idl_hash` was computed from and
/// what gets persisted into `programs.idl_json`. Holding the raw bytes is
/// what gives story 4.4 AC5 (hash stability) its byte-exact guarantee:
/// `compute_idl_hash(raw_json) == hash`. Re-serializing through
/// `serde_json::to_string(&idl)` would silently drop fields not modeled by
/// `anchor_lang_idl_spec::Idl` and shuffle `Option` `None`-vs-absent
/// representations, breaking the round trip for any future feature that
/// re-hashes persisted bytes (e.g., on-chain IDL drift detection).
#[derive(Debug, Clone)]
pub struct CachedIdl {
    pub idl: Idl,
    pub hash: String,
    pub source: IdlSource,
    pub raw_json: String,
}

/// Owned parameters needed for async IDL fetch outside the lock.
///
/// Cloned from `IdlManager` via `fetch_params()`, used with
/// `IdlManager::fetch_idl_standalone()`.
#[derive(Debug, Clone)]
pub struct IdlFetchParams {
    pub rpc_url: String,
    pub http_client: reqwest::Client,
    pub bundled_idls_path: Option<PathBuf>,
}

/// IDL manager: caches, parses, and provides IDL data for programs.
///
/// NOTE: AC6 concurrency (safe for concurrent readers) is handled by the
/// `ProgramRegistry` wrapper in story 2.2 via `Arc<RwLock<ProgramRegistry>>`.
pub struct IdlManager {
    cache: HashMap<String, CachedIdl>,
    rpc_url: String,
    http_client: reqwest::Client,
    bundled_idls_path: Option<PathBuf>,
}

impl IdlManager {
    pub fn new(rpc_url: String) -> Self {
        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self {
            cache: HashMap::new(),
            rpc_url,
            http_client,
            bundled_idls_path: Some(PathBuf::from("idls")),
        }
    }

    /// Retrieve an IDL for a program, using cache-first then fetch cascade.
    ///
    /// Clones `http_client` and `rpc_url` before async fetch to avoid borrowing
    /// `&mut self` across `.await` points — this ensures the returned future is
    /// `Send` when called through `Arc<RwLock<ProgramRegistry>>`.
    #[tracing::instrument(
        name = "idl.get_idl",
        skip(self),
        fields(program_id = program_id),
        level = "debug",
        err(Display)
    )]
    pub async fn get_idl(&mut self, program_id: &str) -> Result<&Idl, IdlError> {
        // contains_key + index: a single `if let` borrow would conflict with
        // the mutable `self.cache.insert()` below (NLL limitation).
        if self.cache.contains_key(program_id) {
            debug!(program_id, "IDL cache hit");
            return Ok(&self.cache[program_id].idl);
        }

        // Clone what we need for async fetch to avoid &mut self borrow across await
        let client = self.http_client.clone();
        let rpc_url = self.rpc_url.clone();
        let bundled_path = self.bundled_idls_path.clone();
        let pid = program_id.to_string();

        // Fetch cascade: on-chain -> bundled (D2: also try bundled on transient errors)
        let (idl_json, source) = match fetch_idl_from_chain(&client, &rpc_url, &pid).await {
            Ok(json) => {
                info!(program_id = %pid, "fetched IDL from on-chain PDA");
                (json, IdlSource::OnChain)
            }
            Err(IdlError::NotFound(_)) => {
                debug!(program_id = %pid, "on-chain IDL not found, trying bundled");
                let json = fetch_idl_from_bundled(bundled_path.as_deref(), &pid)?;
                info!(program_id = %pid, "loaded IDL from bundled directory");
                (json, IdlSource::Bundled)
            }
            Err(e) => {
                // D2: On transient fetch errors, try bundled before giving up
                warn!(program_id = %pid, error = %e, "on-chain fetch failed, trying bundled");
                match fetch_idl_from_bundled(bundled_path.as_deref(), &pid) {
                    Ok(json) => {
                        info!(
                            program_id = %pid,
                            "loaded IDL from bundled directory (after fetch error)"
                        );
                        (json, IdlSource::Bundled)
                    }
                    Err(_) => return Err(e), // propagate original fetch error
                }
            }
        };

        // Validate format before parsing into typed struct
        let raw_value: serde_json::Value =
            serde_json::from_str(&idl_json).map_err(|e| IdlError::ParseFailed(e.to_string()))?;
        validate_idl(&raw_value)?;

        let hash = compute_idl_hash(&idl_json);

        let idl: Idl =
            serde_json::from_value(raw_value).map_err(|e| IdlError::ParseFailed(e.to_string()))?;

        let cached = CachedIdl {
            idl,
            hash,
            source,
            raw_json: idl_json,
        };
        self.cache.insert(program_id.to_string(), cached);

        Ok(&self.cache[program_id].idl)
    }

    /// Read-only cache access — returns the IDL if previously cached.
    pub fn get_cached(&self, program_id: &str) -> Option<&Idl> {
        self.cache.get(program_id).map(|c| &c.idl)
    }

    /// Get the full cached entry (IDL + hash + source) if available.
    pub fn get_cached_entry(&self, program_id: &str) -> Option<&CachedIdl> {
        self.cache.get(program_id)
    }

    /// Return all cached program IDs.
    pub fn cached_program_ids(&self) -> Vec<&str> {
        self.cache.keys().map(|k| k.as_str()).collect()
    }

    /// Remove a cached IDL entry for a program.
    pub fn remove_cached(&mut self, program_id: &str) {
        self.cache.remove(program_id);
    }

    /// Upload an IDL manually: validate, parse, cache with `Manual` source.
    ///
    /// The `idl_json` argument is stored verbatim in `CachedIdl::raw_json` to
    /// preserve the byte-exact hash invariant (`compute_idl_hash(raw_json) == hash`).
    /// Story 4.4 AC5.
    pub fn upload_idl(&mut self, program_id: &str, idl_json: &str) -> Result<&Idl, IdlError> {
        let raw_value: serde_json::Value =
            serde_json::from_str(idl_json).map_err(|e| IdlError::ParseFailed(e.to_string()))?;
        validate_idl(&raw_value)?;

        let hash = compute_idl_hash(idl_json);
        let idl: Idl =
            serde_json::from_value(raw_value).map_err(|e| IdlError::ParseFailed(e.to_string()))?;

        let cached = CachedIdl {
            idl,
            hash,
            source: IdlSource::Manual,
            raw_json: idl_json.to_string(),
        };
        self.cache.insert(program_id.to_string(), cached);

        Ok(&self.cache[program_id].idl)
    }

    /// Clone the fetch parameters needed for a standalone IDL fetch.
    ///
    /// This lets the caller drop the lock on `ProgramRegistry`, perform the
    /// async fetch via `IdlManager::fetch_idl_standalone()`, then re-acquire
    /// the lock and call `insert_fetched_idl()`.
    pub fn fetch_params(&self) -> IdlFetchParams {
        IdlFetchParams {
            rpc_url: self.rpc_url.clone(),
            http_client: self.http_client.clone(),
            bundled_idls_path: self.bundled_idls_path.clone(),
        }
    }

    /// Fetch an IDL via the cascade (on-chain -> bundled) without requiring
    /// `&mut self`. All network and filesystem I/O uses owned parameters.
    ///
    /// Returns the raw IDL JSON string and the source it was fetched from.
    /// The caller should then call `insert_fetched_idl()` under the write lock
    /// to validate, parse, and cache the result.
    #[tracing::instrument(
        name = "idl.fetch_idl_standalone",
        skip(params),
        fields(program_id = program_id),
        level = "info",
        err(Display)
    )]
    pub async fn fetch_idl_standalone(
        params: &IdlFetchParams,
        program_id: &str,
    ) -> Result<(String, IdlSource), IdlError> {
        let pid = program_id.to_string();

        match fetch_idl_from_chain(&params.http_client, &params.rpc_url, &pid).await {
            Ok(json) => {
                info!(program_id = %pid, "fetched IDL from on-chain PDA");
                Ok((json, IdlSource::OnChain))
            }
            Err(IdlError::NotFound(_)) => {
                debug!(program_id = %pid, "on-chain IDL not found, trying bundled");
                let json = fetch_idl_from_bundled(params.bundled_idls_path.as_deref(), &pid)?;
                info!(program_id = %pid, "loaded IDL from bundled directory");
                Ok((json, IdlSource::Bundled))
            }
            Err(e) => {
                // On transient fetch errors, try bundled before giving up
                warn!(program_id = %pid, error = %e, "on-chain fetch failed, trying bundled");
                match fetch_idl_from_bundled(params.bundled_idls_path.as_deref(), &pid) {
                    Ok(json) => {
                        info!(
                            program_id = %pid,
                            "loaded IDL from bundled directory (after fetch error)"
                        );
                        Ok((json, IdlSource::Bundled))
                    }
                    Err(_) => Err(e), // propagate original fetch error
                }
            }
        }
    }

    /// Insert a pre-fetched IDL into the cache (validate, parse, hash, cache).
    ///
    /// Called under the write lock after `fetch_idl_standalone()` completes,
    /// or by the startup auto-start path with bytes loaded from
    /// `programs.idl_json`. The `idl_json` argument is stored verbatim in
    /// `CachedIdl::raw_json` to preserve the byte-exact hash invariant
    /// (`compute_idl_hash(raw_json) == hash`). Story 4.4 AC5.
    pub fn insert_fetched_idl(
        &mut self,
        program_id: &str,
        idl_json: &str,
        source: IdlSource,
    ) -> Result<&Idl, IdlError> {
        let raw_value: serde_json::Value =
            serde_json::from_str(idl_json).map_err(|e| IdlError::ParseFailed(e.to_string()))?;
        validate_idl(&raw_value)?;

        let hash = compute_idl_hash(idl_json);
        let idl: Idl =
            serde_json::from_value(raw_value).map_err(|e| IdlError::ParseFailed(e.to_string()))?;

        let cached = CachedIdl {
            idl,
            hash,
            source,
            raw_json: idl_json.to_string(),
        };
        self.cache.insert(program_id.to_string(), cached);

        Ok(&self.cache[program_id].idl)
    }
}

/// Validate that the IDL JSON has a v0.30+ format (metadata.spec field).
pub fn validate_idl(value: &serde_json::Value) -> Result<(), IdlError> {
    match value.get("metadata").and_then(|m| m.get("spec")) {
        Some(spec) if spec.is_string() => Ok(()),
        Some(_) => Err(IdlError::UnsupportedFormat(
            "metadata.spec must be a string".to_string(),
        )),
        None => Err(IdlError::UnsupportedFormat(
            "missing metadata.spec field — only Anchor IDL v0.30+ is supported".to_string(),
        )),
    }
}

/// Recursively sort all object keys for canonical JSON serialization.
fn canonicalize_json(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => {
            let sorted: serde_json::Map<String, serde_json::Value> = map
                .into_iter()
                .map(|(k, v)| (k, canonicalize_json(v)))
                .collect::<BTreeMap<_, _>>()
                .into_iter()
                .collect();
            serde_json::Value::Object(sorted)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.into_iter().map(canonicalize_json).collect())
        }
        other => other,
    }
}

/// Compute a SHA-256 hash of the IDL JSON for change detection.
///
/// For deterministic hashing, canonicalizes JSON by sorting all object keys
/// recursively before serialization, ensuring identical IDLs with different
/// key ordering produce the same hash.
pub fn compute_idl_hash(idl_json: &str) -> String {
    let normalized = match serde_json::from_str::<serde_json::Value>(idl_json) {
        Ok(value) => {
            let canonical = canonicalize_json(value);
            serde_json::to_string(&canonical).unwrap_or_else(|e| {
                warn!("IDL hash canonicalization failed, using raw input: {e}");
                idl_json.to_string()
            })
        }
        Err(e) => {
            warn!("IDL hash JSON parse failed, using raw input: {e}");
            idl_json.to_string()
        }
    };

    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

/// Errors that can occur during IDL operations.
#[derive(Debug, thiserror::Error)]
pub enum IdlError {
    #[error("failed to fetch IDL for {program_id}: {reason}")]
    FetchFailed { program_id: String, reason: String },

    #[error("failed to parse IDL: {0}")]
    ParseFailed(String),

    #[error("IDL not found: {0}")]
    NotFound(String),

    #[error("unsupported IDL format: {0}")]
    UnsupportedFormat(String),

    #[error("IDL decompression failed: {0}")]
    DecompressionFailed(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_v030_idl_json() -> String {
        serde_json::json!({
            "address": "11111111111111111111111111111111",
            "metadata": {
                "name": "test_program",
                "version": "0.1.0",
                "spec": "0.1.0"
            },
            "instructions": [],
            "accounts": [],
            "types": []
        })
        .to_string()
    }

    #[test]
    fn validate_idl_accepts_v030_format() {
        let value: serde_json::Value =
            serde_json::from_str(&sample_v030_idl_json()).expect("valid json");
        assert!(validate_idl(&value).is_ok());
    }

    #[test]
    fn validate_idl_rejects_missing_spec() {
        let value = serde_json::json!({
            "address": "11111111111111111111111111111111",
            "metadata": {
                "name": "old_program",
                "version": "0.1.0"
            },
            "instructions": []
        });
        let err = validate_idl(&value).unwrap_err();
        assert!(matches!(err, IdlError::UnsupportedFormat(_)));
    }

    #[test]
    fn validate_idl_rejects_non_string_spec() {
        let value = serde_json::json!({
            "address": "11111111111111111111111111111111",
            "metadata": {
                "name": "bad_program",
                "version": "0.1.0",
                "spec": 42
            },
            "instructions": []
        });
        let err = validate_idl(&value).unwrap_err();
        assert!(matches!(err, IdlError::UnsupportedFormat(_)));
    }

    #[test]
    fn compute_idl_hash_is_consistent() {
        let json = sample_v030_idl_json();
        let h1 = compute_idl_hash(&json);
        let h2 = compute_idl_hash(&json);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64); // SHA-256 hex = 64 chars
    }

    #[test]
    fn compute_idl_hash_differs_for_different_input() {
        let json_a = sample_v030_idl_json();
        let json_b = serde_json::json!({
            "address": "22222222222222222222222222222222",
            "metadata": {
                "name": "other_program",
                "version": "0.2.0",
                "spec": "0.1.0"
            },
            "instructions": []
        })
        .to_string();
        assert_ne!(compute_idl_hash(&json_a), compute_idl_hash(&json_b));
    }

    #[test]
    fn idl_manager_cache_returns_none_before_insert() {
        let manager = IdlManager::new("http://localhost:8899".to_string());
        assert!(manager.get_cached("SomeProgramId").is_none());
    }

    #[test]
    fn idl_manager_cache_returns_some_after_upload() {
        let mut manager = IdlManager::new("http://localhost:8899".to_string());
        let json = sample_v030_idl_json();
        manager
            .upload_idl("TestProgram", &json)
            .expect("upload_idl should succeed");
        assert!(manager.get_cached("TestProgram").is_some());
        assert_eq!(
            manager.get_cached("TestProgram").map(|i| &i.metadata.name),
            Some(&"test_program".to_string())
        );
    }

    #[test]
    fn idl_manager_cached_entry_has_correct_source() {
        let mut manager = IdlManager::new("http://localhost:8899".to_string());
        let json = sample_v030_idl_json();
        manager
            .upload_idl("TestProgram", &json)
            .expect("upload_idl should succeed");
        let entry = manager.get_cached_entry("TestProgram");
        assert!(entry.is_some());
        assert_eq!(entry.map(|e| &e.source), Some(&IdlSource::Manual));
    }

    #[test]
    fn upload_idl_rejects_invalid_json() {
        let mut manager = IdlManager::new("http://localhost:8899".to_string());
        let err = manager.upload_idl("prog", "not json").unwrap_err();
        assert!(matches!(err, IdlError::ParseFailed(_)));
    }

    #[test]
    fn upload_idl_rejects_missing_spec() {
        let mut manager = IdlManager::new("http://localhost:8899".to_string());
        let json = serde_json::json!({
            "address": "11111111111111111111111111111111",
            "metadata": {
                "name": "bad",
                "version": "0.1.0"
            },
            "instructions": []
        })
        .to_string();
        let err = manager.upload_idl("prog", &json).unwrap_err();
        assert!(matches!(err, IdlError::UnsupportedFormat(_)));
    }

    #[test]
    fn idl_source_as_str() {
        assert_eq!(IdlSource::OnChain.as_str(), "onchain");
        assert_eq!(IdlSource::Bundled.as_str(), "bundled");
        assert_eq!(IdlSource::Manual.as_str(), "manual");
    }

    // -----------------------------------------------------------------------
    // Send-safety compile-time checks (Story 6.4 AC9)
    //
    // WHY: The project had a recurring `!Send` regression class during Sprint
    // 3 / 4 where an `&mut self` state machine composed with sqlx transactions
    // or RwLock write guards would silently fail Send inference in a composed
    // async state machine. The root-cause lessons are in:
    //
    //   - `_bmad-output/problem-solution-2026-04-06.md`
    //   - MEMORY.md "async Rust + sqlx `!Send` pattern"
    //   - MEMORY.md "cfg(test) not type-checked when crate has errors"
    //
    // PATTERN: Use `fn _check(...) { _require_send(&fut); }` + `let _: fn(...) = _check;`
    // (the fn-pointer cast forces monomorphization of `_check` regardless of
    // test body execution — the `&dyn Send` trick inside the `#[test]` body
    // is NOT safe because rustc can skip an uncompiled test body when the
    // surrounding crate has errors).
    //
    // VERIFICATION PROCEDURE (one-time; re-run if the pattern ever gets
    // refactored): introduce a `std::cell::Cell<u32>` into one of the target
    // futures' signatures, run `cargo check --lib`, confirm the build fails.
    // If it does NOT fail, the pattern is wrong and this test is doing nothing.
    // -----------------------------------------------------------------------

    #[test]
    fn test_get_idl_future_is_send() {
        fn _check(manager: &mut IdlManager, pid: &str) {
            fn _require_send<T: Send>(_: &T) {}
            let fut = manager.get_idl(pid);
            _require_send(&fut);
        }
        let _: fn(&mut IdlManager, &str) = _check;
    }

    #[test]
    fn cached_program_ids_returns_inserted_keys() {
        let mut manager = IdlManager::new("http://localhost:8899".to_string());
        let json = sample_v030_idl_json();
        manager
            .upload_idl("prog_a", &json)
            .expect("upload should succeed");
        manager
            .upload_idl("prog_b", &json)
            .expect("upload should succeed");
        let mut ids = manager.cached_program_ids();
        ids.sort();
        assert_eq!(ids, vec!["prog_a", "prog_b"]);
    }
}
