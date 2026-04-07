// std library
use std::collections::HashMap;
use std::sync::Mutex;

// external crates
use anchor_lang_idl_spec::{
    Idl, IdlArrayLen, IdlDefinedFields, IdlGenericArg, IdlInstruction, IdlSerialization, IdlType,
    IdlTypeDef, IdlTypeDefTy,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tracing::{debug, warn};

// internal crate
use crate::types::{DecodedAccount, DecodedInstruction};

const MAX_DECODE_DEPTH: u32 = 64;

/// Trait for decoding Solana instructions and account data.
///
/// Implementations must be `Send + Sync` for use across async tasks.
pub trait SolarixDecoder: Send + Sync {
    /// Decode a Solana instruction's data bytes using the program's IDL.
    fn decode_instruction(
        &self,
        program_id: &str,
        data: &[u8],
        idl: &Idl,
    ) -> Result<DecodedInstruction, DecodeError>;

    /// Decode a Solana account's data bytes using the program's IDL.
    fn decode_account(
        &self,
        program_id: &str,
        pubkey: &str,
        data: &[u8],
        idl: &Idl,
    ) -> Result<DecodedAccount, DecodeError>;
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

impl DecodeError {
    /// Stable snake_case tag used as `error.kind` on structured log events
    /// (Story 6.1 AC3). The match is deliberately exhaustive (no `_` arm) so
    /// adding a new variant fails the build until this helper is updated.
    pub fn variant_name(&self) -> &'static str {
        match self {
            DecodeError::UnknownDiscriminator(_) => "unknown_discriminator",
            DecodeError::DeserializationFailed(_) => "deserialization_failed",
            DecodeError::IdlNotLoaded(_) => "idl_not_loaded",
            DecodeError::UnsupportedType(_) => "unsupported_type",
        }
    }
}

// ---------------------------------------------------------------------------
// TypeRegistry — resolves named types from IDL
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct TypeRegistry {
    types: HashMap<String, IdlTypeDef>,
}

impl TypeRegistry {
    fn from_idl(idl: &Idl) -> Self {
        let mut types = HashMap::with_capacity(idl.types.len());
        for td in &idl.types {
            types.insert(td.name.clone(), td.clone());
        }
        Self { types }
    }

    fn resolve(&self, name: &str) -> Result<&IdlTypeDef, DecodeError> {
        self.types
            .get(name)
            .ok_or_else(|| DecodeError::DeserializationFailed(format!("unknown type: {name}")))
    }
}

// ---------------------------------------------------------------------------
// Discriminator matching
// ---------------------------------------------------------------------------

fn find_instruction<'a>(data: &[u8], idl: &'a Idl) -> Option<&'a IdlInstruction> {
    for ix in &idl.instructions {
        let disc = &ix.discriminator;
        if disc.is_empty() {
            continue;
        }
        if data.len() >= disc.len() && data[..disc.len()] == disc[..] {
            return Some(ix);
        }
    }
    None
}

fn find_account<'a>(data: &[u8], idl: &'a Idl) -> Option<&'a anchor_lang_idl_spec::IdlAccount> {
    for acct in &idl.accounts {
        let disc = &acct.discriminator;
        if disc.is_empty() {
            continue;
        }
        if data.len() >= disc.len() && data[..disc.len()] == disc[..] {
            return Some(acct);
        }
    }
    None
}

fn find_instruction_with_fallback<'a>(
    data: &[u8],
    idl: &'a Idl,
) -> Result<&'a IdlInstruction, DecodeError> {
    // Try pre-computed discriminators first
    if let Some(ix) = find_instruction(data, idl) {
        return Ok(ix);
    }

    // Fallback: compute SHA-256 discriminators for instructions without them
    for ix in &idl.instructions {
        if !ix.discriminator.is_empty() {
            continue;
        }
        let computed = compute_instruction_discriminator(&ix.name);
        if data.len() >= 8 && data[..8] == computed[..] {
            debug!(instruction = %ix.name, "matched instruction via SHA-256 fallback discriminator");
            return Ok(ix);
        }
    }

    let hex = if data.len() >= 8 {
        hex_encode(&data[..8])
    } else {
        hex_encode(data)
    };
    warn!(discriminator = %hex, "unknown instruction discriminator");
    Err(DecodeError::UnknownDiscriminator(hex))
}

fn compute_instruction_discriminator(name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{name}").as_bytes());
    let hash = hasher.finalize();
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

fn compute_account_discriminator(name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(format!("account:{name}").as_bytes());
    let hash = hasher.finalize();
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

fn find_account_with_fallback<'a>(
    data: &[u8],
    idl: &'a Idl,
) -> Result<&'a anchor_lang_idl_spec::IdlAccount, DecodeError> {
    if let Some(acct) = find_account(data, idl) {
        return Ok(acct);
    }

    // Fallback: compute SHA-256 discriminators for accounts without them
    for acct in &idl.accounts {
        if !acct.discriminator.is_empty() {
            continue;
        }
        let computed = compute_account_discriminator(&acct.name);
        if data.len() >= 8 && data[..8] == computed[..] {
            debug!(account = %acct.name, "matched account via SHA-256 fallback discriminator");
            return Ok(acct);
        }
    }

    let hex = if data.len() >= 8 {
        hex_encode(&data[..8])
    } else {
        hex_encode(data)
    };
    warn!(discriminator = %hex, "unknown account discriminator");
    Err(DecodeError::UnknownDiscriminator(hex))
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Returns true if the failure rate exceeds 90%, indicating likely IDL mismatch.
pub fn is_high_failure_rate(failures: usize, total: usize) -> bool {
    total > 0 && failures.checked_mul(100).is_none_or(|v| v / total > 90)
}

// ---------------------------------------------------------------------------
// Borsh decoder helpers
// ---------------------------------------------------------------------------

fn ensure_bytes(data: &[u8], offset: usize, needed: usize) -> Result<(), DecodeError> {
    if offset
        .checked_add(needed)
        .is_none_or(|end| end > data.len())
    {
        return Err(DecodeError::DeserializationFailed(format!(
            "unexpected EOF: need {needed} bytes at offset {offset}, have {}",
            data.len()
        )));
    }
    Ok(())
}

macro_rules! read_le {
    ($name:ident, $ty:ty, $size:expr) => {
        fn $name(data: &[u8], offset: usize) -> Result<($ty, usize), DecodeError> {
            ensure_bytes(data, offset, $size)?;
            let mut buf = [0u8; $size];
            buf.copy_from_slice(&data[offset..offset + $size]);
            Ok((<$ty>::from_le_bytes(buf), $size))
        }
    };
}

read_le!(read_u8, u8, 1);
read_le!(read_i8, i8, 1);
read_le!(read_u16, u16, 2);
read_le!(read_i16, i16, 2);
read_le!(read_u32, u32, 4);
read_le!(read_i32, i32, 4);
read_le!(read_u64, u64, 8);
read_le!(read_i64, i64, 8);
read_le!(read_f32, f32, 4);
read_le!(read_f64, f64, 8);
read_le!(read_u128, u128, 16);
read_le!(read_i128, i128, 16);

fn format_non_finite_f32(v: f32) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v == f32::INFINITY {
        "Infinity".to_string()
    } else {
        "-Infinity".to_string()
    }
}

fn format_non_finite_f64(v: f64) -> String {
    if v.is_nan() {
        "NaN".to_string()
    } else if v == f64::INFINITY {
        "Infinity".to_string()
    } else {
        "-Infinity".to_string()
    }
}

// ---------------------------------------------------------------------------
// Core recursive type decoder
// ---------------------------------------------------------------------------

fn decode_type(
    data: &[u8],
    offset: usize,
    idl_type: &IdlType,
    registry: &TypeRegistry,
    generics: &HashMap<String, IdlType>,
    depth: u32,
) -> Result<(Value, usize), DecodeError> {
    if depth > MAX_DECODE_DEPTH {
        return Err(DecodeError::DeserializationFailed(
            "max decode depth exceeded".to_string(),
        ));
    }

    match idl_type {
        IdlType::Bool => {
            ensure_bytes(data, offset, 1)?;
            let val = data[offset];
            match val {
                0 => Ok((Value::Bool(false), 1)),
                1 => Ok((Value::Bool(true), 1)),
                _ => Err(DecodeError::DeserializationFailed(format!(
                    "invalid bool byte: {val:#04x}"
                ))),
            }
        }

        IdlType::U8 => {
            let (v, n) = read_u8(data, offset)?;
            Ok((json!(v), n))
        }
        IdlType::I8 => {
            let (v, n) = read_i8(data, offset)?;
            Ok((json!(v), n))
        }
        IdlType::U16 => {
            let (v, n) = read_u16(data, offset)?;
            Ok((json!(v), n))
        }
        IdlType::I16 => {
            let (v, n) = read_i16(data, offset)?;
            Ok((json!(v), n))
        }
        IdlType::U32 => {
            let (v, n) = read_u32(data, offset)?;
            Ok((json!(v), n))
        }
        IdlType::I32 => {
            let (v, n) = read_i32(data, offset)?;
            Ok((json!(v), n))
        }
        IdlType::F32 => {
            let (v, n) = read_f32(data, offset)?;
            if v.is_finite() {
                Ok((json!(v), n))
            } else {
                Ok((Value::String(format_non_finite_f32(v)), n))
            }
        }
        IdlType::U64 => {
            let (v, n) = read_u64(data, offset)?;
            Ok((json!(v), n))
        }
        IdlType::I64 => {
            let (v, n) = read_i64(data, offset)?;
            Ok((json!(v), n))
        }
        IdlType::F64 => {
            let (v, n) = read_f64(data, offset)?;
            if v.is_finite() {
                Ok((json!(v), n))
            } else {
                Ok((Value::String(format_non_finite_f64(v)), n))
            }
        }

        // Large integers -> JSON strings to prevent precision loss
        IdlType::U128 => {
            let (v, n) = read_u128(data, offset)?;
            Ok((Value::String(v.to_string()), n))
        }
        IdlType::I128 => {
            let (v, n) = read_i128(data, offset)?;
            Ok((Value::String(v.to_string()), n))
        }
        IdlType::U256 => {
            ensure_bytes(data, offset, 32)?;
            let bytes = &data[offset..offset + 32];
            let hex = hex_encode(bytes);
            Ok((Value::String(format!("0x{hex}")), 32))
        }
        IdlType::I256 => {
            ensure_bytes(data, offset, 32)?;
            let bytes = &data[offset..offset + 32];
            let hex = hex_encode(bytes);
            Ok((Value::String(format!("0x{hex}")), 32))
        }

        IdlType::String => {
            let (len, _) = read_u32(data, offset)?;
            let len = len as usize;
            ensure_bytes(data, offset + 4, len)?;
            let s = std::str::from_utf8(&data[offset + 4..offset + 4 + len])
                .map_err(|e| DecodeError::DeserializationFailed(format!("invalid UTF-8: {e}")))?;
            Ok((Value::String(s.to_string()), 4 + len))
        }

        IdlType::Bytes => {
            let (len, _) = read_u32(data, offset)?;
            let len = len as usize;
            ensure_bytes(data, offset + 4, len)?;
            let bytes: Vec<Value> = data[offset + 4..offset + 4 + len]
                .iter()
                .map(|&b| json!(b))
                .collect();
            Ok((Value::Array(bytes), 4 + len))
        }

        IdlType::Pubkey => {
            ensure_bytes(data, offset, 32)?;
            let key_bytes = &data[offset..offset + 32];
            let encoded = bs58::encode(key_bytes).into_string();
            Ok((Value::String(encoded), 32))
        }

        IdlType::Option(inner) => {
            ensure_bytes(data, offset, 1)?;
            match data[offset] {
                0 => Ok((Value::Null, 1)),
                1 => {
                    let (val, consumed) =
                        decode_type(data, offset + 1, inner, registry, generics, depth + 1)?;
                    Ok((val, 1 + consumed))
                }
                tag => Err(DecodeError::DeserializationFailed(format!(
                    "invalid Option tag: {tag:#04x}"
                ))),
            }
        }

        IdlType::Vec(inner) => {
            let (count, _) = read_u32(data, offset)?;
            let count = count as usize;
            let mut items = Vec::with_capacity(count.min(1024));
            let mut consumed = 4;
            for _ in 0..count {
                let (val, n) = decode_type(
                    data,
                    offset + consumed,
                    inner,
                    registry,
                    generics,
                    depth + 1,
                )?;
                items.push(val);
                consumed += n;
            }
            Ok((Value::Array(items), consumed))
        }

        IdlType::Array(inner, len) => {
            let count = resolve_array_len(len, generics)?;
            let mut items = Vec::with_capacity(count);
            let mut consumed = 0;
            for _ in 0..count {
                let (val, n) = decode_type(
                    data,
                    offset + consumed,
                    inner,
                    registry,
                    generics,
                    depth + 1,
                )?;
                items.push(val);
                consumed += n;
            }
            Ok((Value::Array(items), consumed))
        }

        IdlType::Defined {
            name,
            generics: generic_args,
        } => {
            // COption<T> — Solana's C-compatible fixed-size option
            if name == "COption" {
                return decode_coption(data, offset, generic_args, registry, generics, depth);
            }

            let typedef = registry.resolve(name)?;
            check_serialization(&typedef.serialization)?;

            // Build generic bindings if any
            let mut local_generics = generics.clone();
            for (def_gen, arg) in typedef.generics.iter().zip(generic_args.iter()) {
                // Const generics handled via IdlArrayLen::Generic; only bind type generics here.
                if let (
                    anchor_lang_idl_spec::IdlTypeDefGeneric::Type { name },
                    IdlGenericArg::Type { ty },
                ) = (def_gen, arg)
                {
                    local_generics.insert(name.clone(), ty.clone());
                }
            }

            decode_typedef(data, offset, typedef, registry, &local_generics, depth + 1)
        }

        IdlType::Generic(name) => {
            if let Some(resolved) = generics.get(name) {
                decode_type(data, offset, resolved, registry, generics, depth + 1)
            } else {
                Err(DecodeError::DeserializationFailed(format!(
                    "unresolved generic: {name}"
                )))
            }
        }

        _ => Err(DecodeError::UnsupportedType(format!(
            "unknown IdlType variant: {idl_type:?}"
        ))),
    }
}

fn resolve_array_len(
    len: &IdlArrayLen,
    generics: &HashMap<String, IdlType>,
) -> Result<usize, DecodeError> {
    match len {
        IdlArrayLen::Value(n) => Ok(*n),
        IdlArrayLen::Generic(name) => {
            // Try to find a const generic binding — check if there's a matching entry
            // that we stored as a string value
            let _ = generics; // const generics come via IdlGenericArg::Const
            Err(DecodeError::DeserializationFailed(format!(
                "unresolved generic array length: {name}"
            )))
        }
    }
}

fn check_serialization(ser: &IdlSerialization) -> Result<(), DecodeError> {
    match ser {
        IdlSerialization::Borsh => Ok(()),
        IdlSerialization::Bytemuck => Err(DecodeError::UnsupportedType(
            "Bytemuck serialization is not supported".to_string(),
        )),
        IdlSerialization::BytemuckUnsafe => Err(DecodeError::UnsupportedType(
            "BytemuckUnsafe serialization is not supported".to_string(),
        )),
        _ => Err(DecodeError::UnsupportedType(
            "non-Borsh serialization is not supported".to_string(),
        )),
    }
}

fn fixed_size(ty: &IdlType, registry: &TypeRegistry) -> Option<usize> {
    fixed_size_inner(ty, registry, 0)
}

fn fixed_size_inner(ty: &IdlType, registry: &TypeRegistry, depth: u32) -> Option<usize> {
    if depth > MAX_DECODE_DEPTH {
        return None;
    }
    match ty {
        IdlType::Bool => Some(1),
        IdlType::U8 | IdlType::I8 => Some(1),
        IdlType::U16 | IdlType::I16 => Some(2),
        IdlType::U32 | IdlType::I32 | IdlType::F32 => Some(4),
        IdlType::U64 | IdlType::I64 | IdlType::F64 => Some(8),
        IdlType::U128 | IdlType::I128 => Some(16),
        IdlType::U256 | IdlType::I256 => Some(32),
        IdlType::Pubkey => Some(32),
        IdlType::Array(inner, IdlArrayLen::Value(n)) => {
            fixed_size_inner(inner, registry, depth + 1).and_then(|s| s.checked_mul(*n))
        }
        IdlType::String | IdlType::Bytes => None,
        IdlType::Vec(_) | IdlType::Option(_) => None,
        IdlType::Defined {
            name,
            generics: generic_args,
        } => {
            // COption<T> — fixed-size: 4 (tag) + fixed_size(T)
            if name == "COption" {
                let inner = match generic_args.first() {
                    Some(IdlGenericArg::Type { ty }) => ty,
                    _ => return None,
                };
                let inner_sz = fixed_size_inner(inner, registry, depth + 1)?;
                return 4usize.checked_add(inner_sz);
            }

            let typedef = registry.types.get(name)?;
            match &typedef.ty {
                IdlTypeDefTy::Struct {
                    fields: Some(IdlDefinedFields::Named(fields)),
                } => {
                    let mut total = 0usize;
                    for f in fields {
                        total = total.checked_add(fixed_size_inner(&f.ty, registry, depth + 1)?)?;
                    }
                    Some(total)
                }
                IdlTypeDefTy::Struct {
                    fields: Some(IdlDefinedFields::Tuple(types)),
                } => {
                    let mut total = 0usize;
                    for t in types {
                        total = total.checked_add(fixed_size_inner(t, registry, depth + 1)?)?;
                    }
                    Some(total)
                }
                IdlTypeDefTy::Type { alias } => fixed_size_inner(alias, registry, depth + 1),
                _ => None,
            }
        }
        _ => None,
    }
}

fn decode_coption(
    data: &[u8],
    offset: usize,
    generic_args: &[IdlGenericArg],
    registry: &TypeRegistry,
    generics: &HashMap<String, IdlType>,
    depth: u32,
) -> Result<(Value, usize), DecodeError> {
    let inner_type = match generic_args.first() {
        Some(IdlGenericArg::Type { ty }) => ty,
        _ => {
            return Err(DecodeError::UnsupportedType(
                "COption requires exactly one type argument".to_string(),
            ))
        }
    };

    let inner_size = fixed_size(inner_type, registry).ok_or_else(|| {
        DecodeError::UnsupportedType("COption with variable-size inner type".to_string())
    })?;

    let (tag, _) = read_u32(data, offset)?;
    let total_consumed = 4 + inner_size;

    match tag {
        0 => {
            // None — skip past tag + fixed inner size
            ensure_bytes(data, offset, total_consumed)?;
            Ok((Value::Null, total_consumed))
        }
        1 => {
            // Some — decode the inner value
            let (val, _) =
                decode_type(data, offset + 4, inner_type, registry, generics, depth + 1)?;
            Ok((val, total_consumed))
        }
        _ => Err(DecodeError::DeserializationFailed(format!(
            "invalid COption tag: {tag}"
        ))),
    }
}

fn decode_typedef(
    data: &[u8],
    offset: usize,
    typedef: &IdlTypeDef,
    registry: &TypeRegistry,
    generics: &HashMap<String, IdlType>,
    depth: u32,
) -> Result<(Value, usize), DecodeError> {
    match &typedef.ty {
        IdlTypeDefTy::Struct { fields } => {
            decode_struct_fields(data, offset, fields, registry, generics, depth)
        }

        IdlTypeDefTy::Enum { variants } => {
            ensure_bytes(data, offset, 1)?;
            let variant_idx = data[offset] as usize;
            let variant = variants.get(variant_idx).ok_or_else(|| {
                DecodeError::DeserializationFailed(format!(
                    "enum variant index {variant_idx} out of range (max {})",
                    variants.len()
                ))
            })?;

            let mut consumed = 1;
            let payload = match &variant.fields {
                None => json!({}),
                Some(IdlDefinedFields::Named(fields)) => {
                    let mut obj = serde_json::Map::new();
                    for field in fields {
                        let (val, n) = decode_type(
                            data,
                            offset + consumed,
                            &field.ty,
                            registry,
                            generics,
                            depth + 1,
                        )?;
                        obj.insert(field.name.clone(), val);
                        consumed += n;
                    }
                    Value::Object(obj)
                }
                Some(IdlDefinedFields::Tuple(types)) => {
                    if types.len() == 1 {
                        let (val, n) = decode_type(
                            data,
                            offset + consumed,
                            &types[0],
                            registry,
                            generics,
                            depth + 1,
                        )?;
                        consumed += n;
                        val
                    } else {
                        let mut arr = Vec::with_capacity(types.len());
                        for ty in types {
                            let (val, n) = decode_type(
                                data,
                                offset + consumed,
                                ty,
                                registry,
                                generics,
                                depth + 1,
                            )?;
                            arr.push(val);
                            consumed += n;
                        }
                        Value::Array(arr)
                    }
                }
            };

            Ok((json!({ variant.name.clone(): payload }), consumed))
        }

        IdlTypeDefTy::Type { alias } => {
            decode_type(data, offset, alias, registry, generics, depth + 1)
        }
    }
}

fn decode_struct_fields(
    data: &[u8],
    offset: usize,
    fields: &Option<IdlDefinedFields>,
    registry: &TypeRegistry,
    generics: &HashMap<String, IdlType>,
    depth: u32,
) -> Result<(Value, usize), DecodeError> {
    match fields {
        None => Ok((json!({}), 0)),
        Some(IdlDefinedFields::Named(fields)) => {
            let mut obj = serde_json::Map::new();
            let mut consumed = 0;
            for field in fields {
                let (val, n) = decode_type(
                    data,
                    offset + consumed,
                    &field.ty,
                    registry,
                    generics,
                    depth + 1,
                )?;
                obj.insert(field.name.clone(), val);
                consumed += n;
            }
            Ok((Value::Object(obj), consumed))
        }
        Some(IdlDefinedFields::Tuple(types)) => {
            let mut arr = Vec::with_capacity(types.len());
            let mut consumed = 0;
            for ty in types {
                let (val, n) =
                    decode_type(data, offset + consumed, ty, registry, generics, depth + 1)?;
                arr.push(val);
                consumed += n;
            }
            Ok((Value::Array(arr), consumed))
        }
    }
}

// ---------------------------------------------------------------------------
// ChainparserDecoder — SolarixDecoder implementation
// ---------------------------------------------------------------------------

/// Decoder that deserializes Borsh-encoded instruction/account data
/// using Anchor IDL type information. Caches TypeRegistry per-IDL for reuse.
pub struct ChainparserDecoder {
    registry_cache: Mutex<HashMap<String, TypeRegistry>>,
}

impl ChainparserDecoder {
    pub fn new() -> Self {
        Self {
            registry_cache: Mutex::new(HashMap::new()),
        }
    }

    fn get_or_build_registry(&self, idl: &Idl) -> TypeRegistry {
        let key = &idl.address;
        let mut cache = self
            .registry_cache
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(existing) = cache.get(key) {
            return existing.clone();
        }
        let registry = TypeRegistry::from_idl(idl);
        cache.insert(key.clone(), registry.clone());
        cache
            .get(key)
            .cloned()
            .unwrap_or_else(|| TypeRegistry::from_idl(idl))
    }
}

impl Default for ChainparserDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl SolarixDecoder for ChainparserDecoder {
    fn decode_instruction(
        &self,
        program_id: &str,
        data: &[u8],
        idl: &Idl,
    ) -> Result<DecodedInstruction, DecodeError> {
        let ix = find_instruction_with_fallback(data, idl)?;
        let registry = self.get_or_build_registry(idl);
        let generics = HashMap::new();

        let disc_len = if ix.discriminator.is_empty() {
            8
        } else {
            ix.discriminator.len()
        };
        let args_data_offset = disc_len;

        let mut args_obj = serde_json::Map::new();
        let mut offset = args_data_offset;

        for field in &ix.args {
            let (val, consumed) = decode_type(data, offset, &field.ty, &registry, &generics, 0)?;
            args_obj.insert(field.name.clone(), val);
            offset += consumed;
        }

        if offset < data.len() {
            debug!(
                program_id,
                instruction = %ix.name,
                trailing_bytes = data.len() - offset,
                "instruction data has trailing bytes after decoding"
            );
        }

        Ok(DecodedInstruction::from_decoded(
            program_id.to_string(),
            ix.name.clone(),
            Value::Object(args_obj),
        ))
    }

    fn decode_account(
        &self,
        program_id: &str,
        pubkey: &str,
        data: &[u8],
        idl: &Idl,
    ) -> Result<DecodedAccount, DecodeError> {
        let account = find_account_with_fallback(data, idl)?;

        let registry = self.get_or_build_registry(idl);
        let type_def = registry.resolve(&account.name)?;

        check_serialization(&type_def.serialization)?;

        let disc_len = if account.discriminator.is_empty() {
            8
        } else {
            account.discriminator.len()
        };

        if data.len() < disc_len {
            return Err(DecodeError::DeserializationFailed(format!(
                "account data too short: {} bytes, need at least {} for discriminator",
                data.len(),
                disc_len
            )));
        }

        let generics = HashMap::new();
        let (value, consumed) = decode_typedef(data, disc_len, type_def, &registry, &generics, 0)?;

        if disc_len + consumed < data.len() {
            debug!(
                program_id,
                account_type = %account.name,
                trailing_bytes = data.len() - disc_len - consumed,
                "account data has trailing bytes after decoding"
            );
        }

        Ok(DecodedAccount::from_decoded(
            program_id.to_string(),
            account.name.clone(),
            pubkey.to_string(),
            value,
        ))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use anchor_lang_idl_spec::*;

    fn make_test_idl(instructions: Vec<IdlInstruction>, types: Vec<IdlTypeDef>) -> Idl {
        Idl {
            address: "11111111111111111111111111111111".to_string(),
            metadata: IdlMetadata {
                name: "test_program".to_string(),
                version: "0.1.0".to_string(),
                spec: "0.1.0".to_string(),
                description: None,
                repository: None,
                dependencies: vec![],
                deployments: None,
                contact: None,
            },
            docs: vec![],
            instructions,
            accounts: vec![],
            events: vec![],
            errors: vec![],
            types,
            constants: vec![],
        }
    }

    fn make_instruction(name: &str, args: Vec<IdlField>) -> IdlInstruction {
        let disc = compute_instruction_discriminator(name);
        IdlInstruction {
            name: name.to_string(),
            docs: vec![],
            discriminator: disc.to_vec(),
            accounts: vec![],
            args,
            returns: None,
        }
    }

    fn make_field(name: &str, ty: IdlType) -> IdlField {
        IdlField {
            name: name.to_string(),
            docs: vec![],
            ty,
        }
    }

    // -- Test: decode primitive instruction args --

    #[test]
    fn test_decode_primitives() {
        let ix = make_instruction(
            "initialize",
            vec![
                make_field("amount", IdlType::U64),
                make_field("flag", IdlType::Bool),
                make_field("count", IdlType::U8),
            ],
        );
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("initialize");
        let mut data = disc.to_vec();
        data.extend_from_slice(&42u64.to_le_bytes()); // amount
        data.push(1); // flag = true
        data.push(7); // count = 7

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.instruction_name, "initialize");
        assert_eq!(result.program_id, "prog1");
        assert_eq!(result.args["amount"], json!(42u64));
        assert_eq!(result.args["flag"], json!(true));
        assert_eq!(result.args["count"], json!(7));
    }

    #[test]
    fn test_decode_string_and_pubkey() {
        let ix = make_instruction(
            "set_name",
            vec![
                make_field("name", IdlType::String),
                make_field("authority", IdlType::Pubkey),
            ],
        );
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("set_name");
        let mut data = disc.to_vec();
        // String: u32 len + UTF-8
        let name_bytes = b"hello";
        data.extend_from_slice(&(name_bytes.len() as u32).to_le_bytes());
        data.extend_from_slice(name_bytes);
        // Pubkey: 32 bytes (all ones for simplicity)
        let pubkey_bytes = [1u8; 32];
        data.extend_from_slice(&pubkey_bytes);

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.args["name"], json!("hello"));
        let expected_pubkey = bs58::encode(&pubkey_bytes).into_string();
        assert_eq!(result.args["authority"], json!(expected_pubkey));
    }

    // -- Test: decode nested struct --

    #[test]
    fn test_decode_nested_struct() {
        let inner_type = IdlTypeDef {
            name: "Config".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![
                    make_field("max_supply", IdlType::U64),
                    make_field("active", IdlType::Bool),
                ])),
            },
        };

        let ix = make_instruction(
            "update_config",
            vec![make_field(
                "config",
                IdlType::Defined {
                    name: "Config".to_string(),
                    generics: vec![],
                },
            )],
        );

        let idl = make_test_idl(vec![ix], vec![inner_type]);

        let disc = compute_instruction_discriminator("update_config");
        let mut data = disc.to_vec();
        data.extend_from_slice(&1000u64.to_le_bytes()); // max_supply
        data.push(1); // active = true

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.args["config"]["max_supply"], json!(1000u64));
        assert_eq!(result.args["config"]["active"], json!(true));
    }

    // -- Test: decode enum variant --

    #[test]
    fn test_decode_enum_variant() {
        let action_type = IdlTypeDef {
            name: "Action".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Enum {
                variants: vec![
                    IdlEnumVariant {
                        name: "Deposit".to_string(),
                        fields: Some(IdlDefinedFields::Named(vec![make_field(
                            "amount",
                            IdlType::U64,
                        )])),
                    },
                    IdlEnumVariant {
                        name: "Withdraw".to_string(),
                        fields: None,
                    },
                ],
            },
        };

        let ix = make_instruction(
            "execute",
            vec![make_field(
                "action",
                IdlType::Defined {
                    name: "Action".to_string(),
                    generics: vec![],
                },
            )],
        );

        let idl = make_test_idl(vec![ix], vec![action_type]);

        // Test Deposit variant (index 0)
        let disc = compute_instruction_discriminator("execute");
        let mut data = disc.to_vec();
        data.push(0); // variant index 0 = Deposit
        data.extend_from_slice(&500u64.to_le_bytes()); // amount

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.args["action"]["Deposit"]["amount"], json!(500u64));

        // Test Withdraw variant (index 1)
        let mut data2 = disc.to_vec();
        data2.push(1); // variant index 1 = Withdraw

        let result2 = decoder
            .decode_instruction("prog1", &data2, &idl)
            .expect("decode should succeed");

        assert_eq!(result2.args["action"]["Withdraw"], json!({}));
    }

    // -- Test: u128/i128 produce JSON strings --

    #[test]
    fn test_large_int_as_string() {
        let ix = make_instruction(
            "big_numbers",
            vec![
                make_field("big_u", IdlType::U128),
                make_field("big_i", IdlType::I128),
            ],
        );
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("big_numbers");
        let mut data = disc.to_vec();
        let big_u: u128 = 340_282_366_920_938_463_463_374_607_431_768_211_455; // u128::MAX
        let big_i: i128 = -170_141_183_460_469_231_731_687_303_715_884_105_728; // i128::MIN
        data.extend_from_slice(&big_u.to_le_bytes());
        data.extend_from_slice(&big_i.to_le_bytes());

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        // Must be strings, not numbers
        assert_eq!(
            result.args["big_u"],
            json!("340282366920938463463374607431768211455")
        );
        assert_eq!(
            result.args["big_i"],
            json!("-170141183460469231731687303715884105728")
        );
    }

    // -- Test: unknown discriminator error --

    #[test]
    fn test_unknown_discriminator() {
        let ix = make_instruction("known", vec![]);
        let idl = make_test_idl(vec![ix], vec![]);

        let data = [0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04];

        let decoder = ChainparserDecoder::new();
        let err = decoder
            .decode_instruction("prog1", &data, &idl)
            .unwrap_err();

        match err {
            DecodeError::UnknownDiscriminator(hex) => {
                assert_eq!(hex, "deadbeef01020304");
            }
            other => panic!("expected UnknownDiscriminator, got: {other}"),
        }
    }

    // -- Test: Bytemuck type returns UnsupportedType --

    #[test]
    fn test_bytemuck_rejection() {
        let bytemuck_type = IdlTypeDef {
            name: "PriceData".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Bytemuck,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![make_field(
                    "price",
                    IdlType::U64,
                )])),
            },
        };

        let ix = make_instruction(
            "read_price",
            vec![make_field(
                "data",
                IdlType::Defined {
                    name: "PriceData".to_string(),
                    generics: vec![],
                },
            )],
        );

        let idl = make_test_idl(vec![ix], vec![bytemuck_type]);

        let disc = compute_instruction_discriminator("read_price");
        let mut data = disc.to_vec();
        data.extend_from_slice(&100u64.to_le_bytes());

        let decoder = ChainparserDecoder::new();
        let err = decoder
            .decode_instruction("prog1", &data, &idl)
            .unwrap_err();

        match err {
            DecodeError::UnsupportedType(msg) => {
                assert!(msg.contains("Bytemuck"));
            }
            other => panic!("expected UnsupportedType, got: {other}"),
        }
    }

    // -- Test: empty args instruction (discriminator only) --

    #[test]
    fn test_empty_args_instruction() {
        let ix = make_instruction("ping", vec![]);
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("ping");
        let data = disc.to_vec();

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.instruction_name, "ping");
        assert_eq!(result.args, json!({}));
    }

    // -- Test: empty IDL accounts returns unknown discriminator error --

    #[test]
    fn test_decode_account_no_accounts_in_idl() {
        let idl = make_test_idl(vec![], vec![]);
        let decoder = ChainparserDecoder::new();
        let err = decoder
            .decode_account("prog1", "pubkey1", &[], &idl)
            .unwrap_err();

        match err {
            DecodeError::UnknownDiscriminator(_) => {
                // Expected: no accounts defined in IDL, so discriminator lookup fails
            }
            other => panic!("expected UnknownDiscriminator, got: {other}"),
        }
    }

    // -- Test: fallback discriminator computation --

    #[test]
    fn test_fallback_discriminator() {
        // Instruction with empty discriminator — should use SHA-256 fallback
        let ix = IdlInstruction {
            name: "legacy_call".to_string(),
            docs: vec![],
            discriminator: vec![], // empty — triggers fallback
            accounts: vec![],
            args: vec![make_field("value", IdlType::U32)],
            returns: None,
        };
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("legacy_call");
        let mut data = disc.to_vec();
        data.extend_from_slice(&99u32.to_le_bytes());

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.instruction_name, "legacy_call");
        assert_eq!(result.args["value"], json!(99));
    }

    // -- Test: Vec and Option decoding --

    #[test]
    fn test_vec_and_option() {
        let ix = make_instruction(
            "batch",
            vec![
                make_field("items", IdlType::Vec(Box::new(IdlType::U16))),
                make_field("label", IdlType::Option(Box::new(IdlType::String))),
            ],
        );
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("batch");
        let mut data = disc.to_vec();
        // Vec<u16>: count=3, [10, 20, 30]
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&10u16.to_le_bytes());
        data.extend_from_slice(&20u16.to_le_bytes());
        data.extend_from_slice(&30u16.to_le_bytes());
        // Option<String>: Some("hi")
        data.push(1); // Some
        data.extend_from_slice(&2u32.to_le_bytes());
        data.extend_from_slice(b"hi");

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.args["items"], json!([10, 20, 30]));
        assert_eq!(result.args["label"], json!("hi"));
    }

    // -- Test: Option None --

    #[test]
    fn test_option_none() {
        let ix = make_instruction(
            "maybe",
            vec![make_field("val", IdlType::Option(Box::new(IdlType::U64)))],
        );
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("maybe");
        let mut data = disc.to_vec();
        data.push(0); // None

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.args["val"], Value::Null);
    }

    // =========================================================================
    // Account decoding tests (Story 3.2)
    // =========================================================================

    fn make_account_idl(accounts: Vec<IdlAccount>, types: Vec<IdlTypeDef>) -> Idl {
        Idl {
            address: "11111111111111111111111111111111".to_string(),
            metadata: IdlMetadata {
                name: "test_program".to_string(),
                version: "0.1.0".to_string(),
                spec: "0.1.0".to_string(),
                description: None,
                repository: None,
                dependencies: vec![],
                deployments: None,
                contact: None,
            },
            docs: vec![],
            instructions: vec![],
            accounts,
            events: vec![],
            errors: vec![],
            types,
            constants: vec![],
        }
    }

    fn make_account_entry(name: &str) -> IdlAccount {
        let disc = compute_account_discriminator(name);
        IdlAccount {
            name: name.to_string(),
            discriminator: disc.to_vec(),
        }
    }

    // -- Test: decode simple account struct (pubkey, u64, bool) --

    #[test]
    fn test_decode_simple_account() {
        let account_type = IdlTypeDef {
            name: "Counter".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![
                    make_field("authority", IdlType::Pubkey),
                    make_field("count", IdlType::U64),
                    make_field("is_active", IdlType::Bool),
                ])),
            },
        };

        let idl = make_account_idl(vec![make_account_entry("Counter")], vec![account_type]);

        let disc = compute_account_discriminator("Counter");
        let mut data = disc.to_vec();
        let authority_bytes = [42u8; 32];
        data.extend_from_slice(&authority_bytes);
        data.extend_from_slice(&99u64.to_le_bytes());
        data.push(1); // is_active = true

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_account("prog1", "acct_pubkey", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.program_id, "prog1");
        assert_eq!(result.account_type, "Counter");
        assert_eq!(result.pubkey, "acct_pubkey");
        let expected_authority = bs58::encode(&authority_bytes).into_string();
        assert_eq!(result.data["authority"], json!(expected_authority));
        assert_eq!(result.data["count"], json!(99u64));
        assert_eq!(result.data["is_active"], json!(true));
    }

    // -- Test: decode account with nested struct --

    #[test]
    fn test_decode_account_nested_struct() {
        let inner = IdlTypeDef {
            name: "Settings".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![
                    make_field("max_supply", IdlType::U64),
                    make_field("active", IdlType::Bool),
                ])),
            },
        };

        let account_type = IdlTypeDef {
            name: "Vault".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![
                    make_field("balance", IdlType::U64),
                    make_field(
                        "settings",
                        IdlType::Defined {
                            name: "Settings".to_string(),
                            generics: vec![],
                        },
                    ),
                ])),
            },
        };

        let idl = make_account_idl(vec![make_account_entry("Vault")], vec![inner, account_type]);

        let disc = compute_account_discriminator("Vault");
        let mut data = disc.to_vec();
        data.extend_from_slice(&500u64.to_le_bytes()); // balance
        data.extend_from_slice(&1000u64.to_le_bytes()); // settings.max_supply
        data.push(0); // settings.active = false

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_account("prog1", "vault_pk", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.data["balance"], json!(500u64));
        assert_eq!(result.data["settings"]["max_supply"], json!(1000u64));
        assert_eq!(result.data["settings"]["active"], json!(false));
    }

    // -- Test: decode account with enum field --

    #[test]
    fn test_decode_account_with_enum() {
        let status_type = IdlTypeDef {
            name: "Status".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Enum {
                variants: vec![
                    IdlEnumVariant {
                        name: "Pending".to_string(),
                        fields: None,
                    },
                    IdlEnumVariant {
                        name: "Active".to_string(),
                        fields: Some(IdlDefinedFields::Named(vec![make_field(
                            "since",
                            IdlType::I64,
                        )])),
                    },
                ],
            },
        };

        let account_type = IdlTypeDef {
            name: "Order".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![
                    make_field("amount", IdlType::U64),
                    make_field(
                        "status",
                        IdlType::Defined {
                            name: "Status".to_string(),
                            generics: vec![],
                        },
                    ),
                ])),
            },
        };

        let idl = make_account_idl(
            vec![make_account_entry("Order")],
            vec![status_type, account_type],
        );

        let disc = compute_account_discriminator("Order");
        let mut data = disc.to_vec();
        data.extend_from_slice(&250u64.to_le_bytes()); // amount
        data.push(1); // variant 1 = Active
        data.extend_from_slice(&1700000000i64.to_le_bytes()); // since

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_account("prog1", "order_pk", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.data["amount"], json!(250u64));
        assert_eq!(
            result.data["status"]["Active"]["since"],
            json!(1700000000i64)
        );
    }

    // -- Test: decode account with Option and Vec fields --

    #[test]
    fn test_decode_account_option_vec() {
        let account_type = IdlTypeDef {
            name: "Listing".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![
                    make_field("owner", IdlType::Option(Box::new(IdlType::Pubkey))),
                    make_field("tags", IdlType::Vec(Box::new(IdlType::U8))),
                ])),
            },
        };

        let idl = make_account_idl(vec![make_account_entry("Listing")], vec![account_type]);

        let disc = compute_account_discriminator("Listing");
        let mut data = disc.to_vec();
        // Option<Pubkey>: Some(all-5s)
        data.push(1);
        let owner_bytes = [5u8; 32];
        data.extend_from_slice(&owner_bytes);
        // Vec<u8>: [10, 20, 30]
        data.extend_from_slice(&3u32.to_le_bytes());
        data.extend_from_slice(&[10, 20, 30]);

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_account("prog1", "listing_pk", &data, &idl)
            .expect("decode should succeed");

        let expected_owner = bs58::encode(&owner_bytes).into_string();
        assert_eq!(result.data["owner"], json!(expected_owner));
        assert_eq!(result.data["tags"], json!([10, 20, 30]));
    }

    // -- Test: COption<Pubkey> with Some value --

    #[test]
    fn test_coption_pubkey_some() {
        let account_type = IdlTypeDef {
            name: "TokenMint".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![make_field(
                    "freeze_authority",
                    IdlType::Defined {
                        name: "COption".to_string(),
                        generics: vec![IdlGenericArg::Type {
                            ty: IdlType::Pubkey,
                        }],
                    },
                )])),
            },
        };

        let idl = make_account_idl(vec![make_account_entry("TokenMint")], vec![account_type]);

        let disc = compute_account_discriminator("TokenMint");
        let mut data = disc.to_vec();
        // COption<Pubkey> Some: u32 tag=1 + 32 bytes pubkey
        data.extend_from_slice(&1u32.to_le_bytes());
        let authority_bytes = [7u8; 32];
        data.extend_from_slice(&authority_bytes);

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_account("prog1", "mint_pk", &data, &idl)
            .expect("decode should succeed");

        let expected = bs58::encode(&authority_bytes).into_string();
        assert_eq!(result.data["freeze_authority"], json!(expected));
    }

    // -- Test: COption<Pubkey> with None value (verify fixed-size skip) --

    #[test]
    fn test_coption_pubkey_none() {
        let account_type = IdlTypeDef {
            name: "TokenMint".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![
                    make_field(
                        "freeze_authority",
                        IdlType::Defined {
                            name: "COption".to_string(),
                            generics: vec![IdlGenericArg::Type {
                                ty: IdlType::Pubkey,
                            }],
                        },
                    ),
                    make_field("supply", IdlType::U64),
                ])),
            },
        };

        let idl = make_account_idl(vec![make_account_entry("TokenMint")], vec![account_type]);

        let disc = compute_account_discriminator("TokenMint");
        let mut data = disc.to_vec();
        // COption<Pubkey> None: u32 tag=0 + 32 zero bytes
        data.extend_from_slice(&0u32.to_le_bytes());
        data.extend_from_slice(&[0u8; 32]);
        // supply field follows
        data.extend_from_slice(&1000000u64.to_le_bytes());

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_account("prog1", "mint_pk", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.data["freeze_authority"], Value::Null);
        assert_eq!(result.data["supply"], json!(1000000u64));
    }

    // -- Test: unknown account discriminator returns correct error --

    #[test]
    fn test_unknown_account_discriminator() {
        let account_type = IdlTypeDef {
            name: "Counter".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![make_field(
                    "count",
                    IdlType::U64,
                )])),
            },
        };

        let idl = make_account_idl(vec![make_account_entry("Counter")], vec![account_type]);

        // Data with wrong discriminator
        let data = [
            0xDE, 0xAD, 0xBE, 0xEF, 0x01, 0x02, 0x03, 0x04, 0x00, 0x00, 0x00, 0x00,
        ];
        let decoder = ChainparserDecoder::new();
        let err = decoder
            .decode_account("prog1", "bad_pk", &data, &idl)
            .unwrap_err();

        match err {
            DecodeError::UnknownDiscriminator(hex) => {
                assert_eq!(hex, "deadbeef01020304");
            }
            other => panic!("expected UnknownDiscriminator, got: {other}"),
        }
    }

    // -- Test: account name not found in types[] returns error --

    #[test]
    fn test_account_type_not_in_types() {
        // Account entry exists but no matching type definition
        let idl = make_account_idl(
            vec![make_account_entry("MissingType")],
            vec![], // no types!
        );

        let disc = compute_account_discriminator("MissingType");
        let mut data = disc.to_vec();
        data.extend_from_slice(&[0u8; 8]);

        let decoder = ChainparserDecoder::new();
        let err = decoder
            .decode_account("prog1", "pk", &data, &idl)
            .unwrap_err();

        match err {
            DecodeError::DeserializationFailed(msg) => {
                assert!(msg.contains("unknown type"));
                assert!(msg.contains("MissingType"));
            }
            other => panic!("expected DeserializationFailed, got: {other}"),
        }
    }

    // -- Test: is_high_failure_rate threshold logic --

    #[test]
    fn test_is_high_failure_rate() {
        // 0 total -> false
        assert!(!is_high_failure_rate(0, 0));
        // 91/100 = 91% > 90 -> true
        assert!(is_high_failure_rate(91, 100));
        // 90/100 = 90% NOT > 90 -> false
        assert!(!is_high_failure_rate(90, 100));
        // 10/10 = 100% -> true
        assert!(is_high_failure_rate(10, 10));
        // 1/10 = 10% -> false
        assert!(!is_high_failure_rate(1, 10));
        // 9/10 = 90% NOT > 90 -> false
        assert!(!is_high_failure_rate(9, 10));
        // Edge: 1/1 = 100% -> true
        assert!(is_high_failure_rate(1, 1));
    }

    // -- Test: account discriminator SHA-256 fallback --

    #[test]
    fn test_account_discriminator_fallback() {
        let account_type = IdlTypeDef {
            name: "Counter".to_string(),
            docs: vec![],
            serialization: IdlSerialization::Borsh,
            repr: None,
            generics: vec![],
            ty: IdlTypeDefTy::Struct {
                fields: Some(IdlDefinedFields::Named(vec![make_field(
                    "count",
                    IdlType::U64,
                )])),
            },
        };

        // Account entry with EMPTY discriminator — triggers SHA-256 fallback
        let account_entry = IdlAccount {
            name: "Counter".to_string(),
            discriminator: vec![],
        };

        let idl = make_account_idl(vec![account_entry], vec![account_type]);

        let disc = compute_account_discriminator("Counter");
        let mut data = disc.to_vec();
        data.extend_from_slice(&42u64.to_le_bytes());

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_account("prog1", "counter_pk", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.account_type, "Counter");
        assert_eq!(result.data["count"], json!(42u64));
    }

    // -- Test: f32/f64 non-finite values produce JSON strings (AC6) --

    #[test]
    fn test_f32_nan_infinity() {
        let ix = make_instruction(
            "floats",
            vec![
                make_field("nan_val", IdlType::F32),
                make_field("pos_inf", IdlType::F32),
                make_field("neg_inf", IdlType::F32),
            ],
        );
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("floats");
        let mut data = disc.to_vec();
        data.extend_from_slice(&f32::NAN.to_le_bytes());
        data.extend_from_slice(&f32::INFINITY.to_le_bytes());
        data.extend_from_slice(&f32::NEG_INFINITY.to_le_bytes());

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.args["nan_val"], json!("NaN"));
        assert_eq!(result.args["pos_inf"], json!("Infinity"));
        assert_eq!(result.args["neg_inf"], json!("-Infinity"));
    }

    #[test]
    fn test_f64_nan_infinity() {
        let ix = make_instruction(
            "doubles",
            vec![
                make_field("nan_val", IdlType::F64),
                make_field("pos_inf", IdlType::F64),
                make_field("neg_inf", IdlType::F64),
            ],
        );
        let idl = make_test_idl(vec![ix], vec![]);

        let disc = compute_instruction_discriminator("doubles");
        let mut data = disc.to_vec();
        data.extend_from_slice(&f64::NAN.to_le_bytes());
        data.extend_from_slice(&f64::INFINITY.to_le_bytes());
        data.extend_from_slice(&f64::NEG_INFINITY.to_le_bytes());

        let decoder = ChainparserDecoder::new();
        let result = decoder
            .decode_instruction("prog1", &data, &idl)
            .expect("decode should succeed");

        assert_eq!(result.args["nan_val"], json!("NaN"));
        assert_eq!(result.args["pos_inf"], json!("Infinity"));
        assert_eq!(result.args["neg_inf"], json!("-Infinity"));
    }

    // -- Test: COption with variable-size inner type errors --

    #[test]
    fn test_coption_variable_size_inner_errors() {
        let registry = TypeRegistry {
            types: HashMap::new(),
        };
        let generics = HashMap::new();

        // COption<String> — String is variable-size, should fail
        let coption_string = IdlType::Defined {
            name: "COption".to_string(),
            generics: vec![IdlGenericArg::Type {
                ty: IdlType::String,
            }],
        };

        let data = [0u8; 40];
        let err = decode_type(&data, 0, &coption_string, &registry, &generics, 0).unwrap_err();
        match err {
            DecodeError::UnsupportedType(msg) => {
                assert!(msg.contains("variable-size"));
            }
            other => panic!("expected UnsupportedType, got: {other}"),
        }
    }

    // -----------------------------------------------------------------------
    // Story 6.1 AC3 — variant_name() contract
    //
    // Every DecodeError variant must map to a unique, non-empty snake_case
    // tag used as `error.kind` on structured log events. The exhaustive match
    // in `DecodeError::variant_name()` prevents adding a new variant without
    // updating this helper.
    // -----------------------------------------------------------------------

    #[test]
    fn decode_error_variant_name_is_unique_and_non_empty() {
        let variants: Vec<&'static str> = vec![
            DecodeError::UnknownDiscriminator("ff".into()).variant_name(),
            DecodeError::DeserializationFailed("bad".into()).variant_name(),
            DecodeError::IdlNotLoaded("prog".into()).variant_name(),
            DecodeError::UnsupportedType("weird".into()).variant_name(),
        ];

        for name in &variants {
            assert!(!name.is_empty(), "variant_name must not be empty");
            assert!(
                name.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "variant_name must be snake_case, got: {name}"
            );
        }

        let mut sorted = variants.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(
            sorted.len(),
            variants.len(),
            "variant_name values must be unique across all variants"
        );
    }
}
