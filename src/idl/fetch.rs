use super::IdlError;

/// Fetch an IDL for a program by its address.
///
/// Implements a cascade: on-chain PDA -> bundled registry -> manual upload.
pub async fn fetch_idl(_program_id: &str) -> Result<serde_json::Value, IdlError> {
    Err(IdlError::NotFound(
        "IDL fetch not yet implemented".to_string(),
    ))
}
