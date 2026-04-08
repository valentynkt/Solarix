//! IDL fetch cascade: on-chain PDA → bundled registry → manual upload.

use std::io::Read;
use std::path::Path;

use base64::Engine;
use flate2::read::ZlibDecoder;
use solana_pubkey::Pubkey;
use tracing::{debug, trace};

use super::IdlError;

/// Maximum decompressed IDL size (16 MiB) to guard against zip bombs.
const MAX_IDL_SIZE: u64 = 16 * 1024 * 1024;

/// Extract a safe "host" portion from an RPC URL for structured log fields.
///
/// Strips scheme, credentials, and query strings so RPC tokens embedded in
/// URLs cannot leak into logs. Story 6.1 AC1 "Sensitive field redaction".
fn rpc_url_host(rpc_url: &str) -> String {
    let after_scheme = rpc_url.split_once("://").map(|(_, r)| r).unwrap_or(rpc_url);
    let without_creds = after_scheme
        .split_once('@')
        .map(|(_, r)| r)
        .unwrap_or(after_scheme);
    without_creds
        .split(['/', '?', '#'])
        .next()
        .unwrap_or("")
        .to_string()
}

/// Fetch an IDL from on-chain PDA via `getAccountInfo` RPC call.
///
/// Derives the IDL PDA using `["anchor:idl", program_id]` seeds, fetches the
/// account data, then decompresses the zlib-compressed IDL JSON payload.
#[tracing::instrument(
    name = "idl.fetch_idl_from_chain",
    skip(client),
    fields(program_id = program_id, rpc_url = %rpc_url_host(rpc_url)),
    level = "info",
    err(Display)
)]
pub async fn fetch_idl_from_chain(
    client: &reqwest::Client,
    rpc_url: &str,
    program_id: &str,
) -> Result<String, IdlError> {
    let program_pubkey = program_id
        .parse::<Pubkey>()
        .map_err(|e| IdlError::FetchFailed {
            program_id: program_id.to_string(),
            reason: format!("invalid program ID: {e}"),
        })?;

    // Anchor IDL account address derivation (matches anchor-lang v0.30 IdlAccount::address):
    //   1. program_signer = find_program_address(&[], program_id).0
    //   2. idl_address    = create_with_seed(&program_signer, "anchor:idl", program_id)
    let (program_signer, _bump) = Pubkey::find_program_address(&[], &program_pubkey);
    let idl_address = Pubkey::create_with_seed(&program_signer, "anchor:idl", &program_pubkey)
        .map_err(|e| IdlError::FetchFailed {
            program_id: program_id.to_string(),
            reason: format!("failed to derive IDL address: {e}"),
        })?;
    let pda_base58 = idl_address.to_string();
    debug!(program_id, idl_address = %pda_base58, "derived IDL account address");

    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "getAccountInfo",
        "params": [
            pda_base58,
            {
                "encoding": "base64",
                "commitment": "confirmed"
            }
        ]
    });

    let response =
        client
            .post(rpc_url)
            .json(&body)
            .send()
            .await
            .map_err(|e| IdlError::FetchFailed {
                program_id: program_id.to_string(),
                reason: e.to_string(),
            })?;

    // P4: Check HTTP status before attempting JSON parse
    let status = response.status();
    if !status.is_success() {
        return Err(IdlError::FetchFailed {
            program_id: program_id.to_string(),
            reason: format!("RPC returned HTTP {status}"),
        });
    }

    let response_json: serde_json::Value =
        response.json().await.map_err(|e| IdlError::FetchFailed {
            program_id: program_id.to_string(),
            reason: format!("failed to parse RPC response: {e}"),
        })?;

    // P5: Check for RPC-level errors (skip null — some RPCs set "error": null on success)
    if let Some(error) = response_json.get("error") {
        if !error.is_null() {
            return Err(IdlError::FetchFailed {
                program_id: program_id.to_string(),
                reason: format!("RPC error: {error}"),
            });
        }
    }

    // P6: Distinguish missing "result" key (malformed) from null value (account not found)
    let result = response_json
        .get("result")
        .ok_or_else(|| IdlError::FetchFailed {
            program_id: program_id.to_string(),
            reason: "malformed RPC response: missing 'result' field".to_string(),
        })?;

    let value = result
        .get("value")
        .filter(|v| !v.is_null())
        .ok_or_else(|| {
            IdlError::NotFound(format!(
                "on-chain IDL account not found for program {program_id}"
            ))
        })?;

    // Extract base64-encoded account data
    let data_b64 = value
        .get("data")
        .and_then(|d| d.as_array())
        .and_then(|arr| arr.first())
        .and_then(|s| s.as_str())
        .ok_or_else(|| IdlError::FetchFailed {
            program_id: program_id.to_string(),
            reason: "missing or invalid data field in account info".to_string(),
        })?;

    let account_data = base64::engine::general_purpose::STANDARD
        .decode(data_b64)
        .map_err(|e| IdlError::FetchFailed {
            program_id: program_id.to_string(),
            reason: format!("base64 decode failed: {e}"),
        })?;

    trace!(
        program_id,
        account_data_len = account_data.len(),
        "decoded account data"
    );

    decompress_idl_data(&account_data)
}

/// Parse the IDL account binary layout and decompress the zlib payload.
///
/// Layout:
/// ```text
/// [0..8]    - 8-byte discriminator
/// [8..40]   - 32-byte authority pubkey
/// [40..44]  - 4-byte LE u32 data_len
/// [44..44+data_len] - zlib-compressed IDL JSON
/// ```
pub fn decompress_idl_data(account_data: &[u8]) -> Result<String, IdlError> {
    const HEADER_SIZE: usize = 8 + 32 + 4; // discriminator + authority + data_len

    if account_data.len() < HEADER_SIZE {
        return Err(IdlError::DecompressionFailed(format!(
            "account data too short: {} bytes, need at least {HEADER_SIZE}",
            account_data.len()
        )));
    }

    // Skip 8-byte discriminator + 32-byte authority, read 4-byte LE data_len
    let data_len_bytes: [u8; 4] = account_data[40..44]
        .try_into()
        .map_err(|_| IdlError::DecompressionFailed("failed to read data_len".to_string()))?;
    let data_len = u32::from_le_bytes(data_len_bytes) as usize;

    let payload_end = HEADER_SIZE
        .checked_add(data_len)
        .ok_or_else(|| IdlError::DecompressionFailed(format!("data_len overflow: {data_len}")))?;
    if account_data.len() < payload_end {
        return Err(IdlError::DecompressionFailed(format!(
            "account data truncated: have {} bytes, need {payload_end} (data_len={data_len})",
            account_data.len()
        )));
    }

    let compressed = &account_data[HEADER_SIZE..payload_end];

    let mut decoder = ZlibDecoder::new(compressed).take(MAX_IDL_SIZE);
    let mut idl_json = String::new();
    decoder
        .read_to_string(&mut idl_json)
        .map_err(|e| IdlError::DecompressionFailed(e.to_string()))?;

    if idl_json.len() as u64 >= MAX_IDL_SIZE {
        return Err(IdlError::DecompressionFailed(format!(
            "decompressed IDL exceeds maximum size of {MAX_IDL_SIZE} bytes"
        )));
    }

    Ok(idl_json)
}

/// Load an IDL from the bundled `idls/` directory.
///
/// Searches for a file named `{program_id}.json` in the bundled IDL path.
pub fn fetch_idl_from_bundled(
    bundled_path: Option<&Path>,
    program_id: &str,
) -> Result<String, IdlError> {
    // P2: Validate program_id doesn't contain path traversal characters
    if program_id.contains('/') || program_id.contains('\\') || program_id.contains("..") {
        return Err(IdlError::NotFound(format!(
            "invalid program ID for bundled lookup: {program_id}"
        )));
    }

    let dir = bundled_path.unwrap_or_else(|| Path::new("idls"));
    let file_path = dir.join(format!("{program_id}.json"));

    // P9: Read directly and map ErrorKind::NotFound, avoiding TOCTOU race
    match std::fs::read_to_string(&file_path) {
        Ok(contents) => Ok(contents),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(IdlError::NotFound(format!(
            "IDL not found for program {program_id}. Upload manually via POST /api/programs"
        ))),
        Err(e) => Err(IdlError::FetchFailed {
            program_id: program_id.to_string(),
            reason: format!("failed to read bundled IDL file: {e}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn idl_address_derivation_is_deterministic() {
        // Known program ID
        let program_id = "TokenkegQfeZyiNwAJbNbGKPFXCWuBvf9Ss623VQ5DA";
        let program_pubkey: Pubkey = program_id.parse().expect("valid pubkey");

        let derive = |pid: &Pubkey| -> Pubkey {
            let (signer, _) = Pubkey::find_program_address(&[], pid);
            Pubkey::create_with_seed(&signer, "anchor:idl", pid).expect("derive idl address")
        };

        let addr1 = derive(&program_pubkey);
        let addr2 = derive(&program_pubkey);
        assert_eq!(addr1, addr2);
        // Address should differ from the program id itself
        assert_ne!(addr1.to_string(), program_id);
    }

    #[test]
    fn decompress_idl_data_with_valid_payload() {
        use flate2::write::ZlibEncoder;
        use flate2::Compression;
        use std::io::Write;

        let idl_json = r#"{"address":"11111111111111111111111111111111","metadata":{"name":"test","version":"0.1.0","spec":"0.1.0"},"instructions":[]}"#;

        // Compress
        let mut encoder = ZlibEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(idl_json.as_bytes()).expect("write");
        let compressed = encoder.finish().expect("finish");

        // Build account data: 8 discriminator + 32 authority + 4 data_len + compressed
        let data_len = compressed.len() as u32;
        let mut account_data = Vec::new();
        account_data.extend_from_slice(&[0u8; 8]); // discriminator
        account_data.extend_from_slice(&[0u8; 32]); // authority
        account_data.extend_from_slice(&data_len.to_le_bytes());
        account_data.extend_from_slice(&compressed);

        let result = decompress_idl_data(&account_data).expect("should decompress");
        assert_eq!(result, idl_json);
    }

    #[test]
    fn decompress_idl_data_rejects_short_data() {
        let short = vec![0u8; 10];
        let err = decompress_idl_data(&short).unwrap_err();
        assert!(matches!(err, IdlError::DecompressionFailed(_)));
    }

    #[test]
    fn decompress_idl_data_rejects_truncated_payload() {
        // Header claims 1000 bytes but only 5 are provided
        let mut account_data = Vec::new();
        account_data.extend_from_slice(&[0u8; 8]); // discriminator
        account_data.extend_from_slice(&[0u8; 32]); // authority
        account_data.extend_from_slice(&1000u32.to_le_bytes());
        account_data.extend_from_slice(&[0u8; 5]); // only 5 bytes
        let err = decompress_idl_data(&account_data).unwrap_err();
        assert!(matches!(err, IdlError::DecompressionFailed(_)));
    }

    #[test]
    fn rpc_response_null_value_means_not_found() {
        // Simulate parsing the RPC response where value is null
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "context": { "slot": 100 },
                "value": null
            }
        });

        let value = response.get("result").and_then(|r| r.get("value"));

        assert!(value.is_some_and(|v| v.is_null()));
    }

    #[test]
    fn rpc_response_with_data_extracts_base64() {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "result": {
                "context": { "slot": 100 },
                "value": {
                    "data": ["AQIDBA==", "base64"],
                    "executable": false,
                    "lamports": 1000000,
                    "owner": "11111111111111111111111111111111",
                    "rentEpoch": 0
                }
            }
        });

        let data_b64 = response
            .get("result")
            .and_then(|r| r.get("value"))
            .and_then(|v| v.get("data"))
            .and_then(|d| d.as_array())
            .and_then(|arr| arr.first())
            .and_then(|s| s.as_str());

        assert_eq!(data_b64, Some("AQIDBA=="));
    }

    #[test]
    fn bundled_not_found_returns_error() {
        let err =
            fetch_idl_from_bundled(Some(Path::new("/nonexistent")), "SomeProgram").unwrap_err();
        assert!(matches!(err, IdlError::NotFound(_)));
    }
}
