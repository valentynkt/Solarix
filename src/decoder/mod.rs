/// Trait for decoding Solana instructions and account data.
///
/// Implementations must be `Send + Sync` for use across async tasks.
pub trait SolarixDecoder: Send + Sync {
    /// Decode a Solana instruction's data bytes into a JSON value.
    fn decode_instruction(
        &self,
        program_id: &str,
        data: &[u8],
    ) -> Result<serde_json::Value, DecodeError>;

    /// Decode a Solana account's data bytes into a JSON value.
    fn decode_account(
        &self,
        program_id: &str,
        data: &[u8],
    ) -> Result<serde_json::Value, DecodeError>;
}

/// Errors that can occur during decoding.
#[derive(Debug, thiserror::Error)]
pub enum DecodeError {
    #[error("unknown discriminator: {0}")]
    UnknownDiscriminator(String),

    #[error("deserialization failed: {0}")]
    DeserializationFailed(String),

    #[error("IDL not loaded for program: {0}")]
    IdlNotLoaded(String),

    #[error("unsupported type: {0}")]
    UnsupportedType(String),
}
