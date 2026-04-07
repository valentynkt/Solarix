// Decoder fixture builders re-used across the integration test crates.
//
// These mirror the `make_test_idl`, `make_instruction`, `make_field`,
// `make_account_idl`, and `make_account_entry` helpers in
// `src/decoder/mod.rs`'s private `#[cfg(test)]` block. Story 6.4 keeps them
// here (instead of exposing the in-crate helpers as `pub(crate)`) so the
// public surface area of `solarix::decoder` does not gain test-only knobs and
// so multiple `tests/*.rs` files can share a single canonical builder.
//
// Source-of-truth pattern: src/decoder/mod.rs:885-1869.

#![allow(dead_code)]

use anchor_lang_idl_spec::{
    Idl, IdlAccount, IdlDefinedFields, IdlField, IdlInstruction, IdlMetadata, IdlSerialization,
    IdlType, IdlTypeDef, IdlTypeDefTy,
};
use sha2::{Digest, Sha256};

/// Compute the 8-byte Anchor instruction discriminator: SHA-256("global:{name}")[..8].
pub fn compute_instruction_discriminator(name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(format!("global:{name}").as_bytes());
    let hash = hasher.finalize();
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

/// Compute the 8-byte Anchor account discriminator: SHA-256("account:{name}")[..8].
pub fn compute_account_discriminator(name: &str) -> [u8; 8] {
    let mut hasher = Sha256::new();
    hasher.update(format!("account:{name}").as_bytes());
    let hash = hasher.finalize();
    let mut disc = [0u8; 8];
    disc.copy_from_slice(&hash[..8]);
    disc
}

pub fn make_field(name: &str, ty: IdlType) -> IdlField {
    IdlField {
        name: name.to_string(),
        docs: vec![],
        ty,
    }
}

pub fn make_instruction(name: &str, args: Vec<IdlField>) -> IdlInstruction {
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

pub fn make_test_idl(instructions: Vec<IdlInstruction>, types: Vec<IdlTypeDef>) -> Idl {
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

pub fn make_account_entry(name: &str) -> IdlAccount {
    let disc = compute_account_discriminator(name);
    IdlAccount {
        name: name.to_string(),
        discriminator: disc.to_vec(),
    }
}

pub fn make_account_idl(accounts: Vec<IdlAccount>, types: Vec<IdlTypeDef>) -> Idl {
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

/// Build a one-arg instruction IDL + raw payload bytes for an arbitrary
/// Borsh-serialized value. Returns `(data_bytes, idl)`.
///
/// The instruction is named `arg_name` so different proptest cases get
/// distinct discriminators (and thus do not collide on hash).
pub fn build_single_arg_instruction(
    arg_name: &str,
    arg_type: IdlType,
    serialized_value: &[u8],
) -> (Vec<u8>, Idl) {
    let ix_name = format!("ix_{arg_name}");
    let ix = make_instruction(&ix_name, vec![make_field(arg_name, arg_type)]);
    let mut data = ix.discriminator.clone();
    data.extend_from_slice(serialized_value);
    let idl = make_test_idl(vec![ix], vec![]);
    (data, idl)
}

/// Build a single-field account IDL + raw account data bytes.
/// Account name follows `Acct{type_tag}` so distinct fields get distinct discriminators.
pub fn build_single_field_account(
    type_tag: &str,
    field_name: &str,
    field_type: IdlType,
    serialized_value: &[u8],
) -> (Vec<u8>, Idl) {
    let acct_name = format!("Acct{type_tag}");
    let account_type = IdlTypeDef {
        name: acct_name.clone(),
        docs: vec![],
        serialization: IdlSerialization::Borsh,
        repr: None,
        generics: vec![],
        ty: IdlTypeDefTy::Struct {
            fields: Some(IdlDefinedFields::Named(vec![make_field(
                field_name, field_type,
            )])),
        },
    };
    let entry = make_account_entry(&acct_name);
    let mut data = entry.discriminator.clone();
    data.extend_from_slice(serialized_value);
    let idl = make_account_idl(vec![entry], vec![account_type]);
    (data, idl)
}

// ---------------------------------------------------------------------------
// Borsh hand-encoders.
//
// We deliberately do NOT pull `borsh` into dev-dependencies — every Borsh
// primitive used by Anchor is little-endian and trivially serializable in a
// few lines. This keeps the test crate's compile time low and pins the wire
// format expectations next to the assertions that consume them.
// ---------------------------------------------------------------------------

pub fn borsh_bool(v: bool) -> Vec<u8> {
    vec![if v { 1 } else { 0 }]
}

pub fn borsh_u8(v: u8) -> Vec<u8> {
    vec![v]
}
pub fn borsh_i8(v: i8) -> Vec<u8> {
    vec![v as u8]
}
pub fn borsh_u16(v: u16) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_i16(v: i16) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_u32(v: u32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_i32(v: i32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_u64(v: u64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_i64(v: i64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_u128(v: u128) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_i128(v: i128) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_f32(v: f32) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}
pub fn borsh_f64(v: f64) -> Vec<u8> {
    v.to_le_bytes().to_vec()
}

pub fn borsh_string(s: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(4 + s.len());
    out.extend_from_slice(&(s.len() as u32).to_le_bytes());
    out.extend_from_slice(s.as_bytes());
    out
}

pub fn borsh_pubkey(bytes: &[u8; 32]) -> Vec<u8> {
    bytes.to_vec()
}

pub fn borsh_option_some(inner: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + inner.len());
    out.push(1);
    out.extend_from_slice(inner);
    out
}

pub fn borsh_option_none() -> Vec<u8> {
    vec![0]
}

pub fn borsh_vec<F>(items: usize, mut encode_item: F) -> Vec<u8>
where
    F: FnMut(usize) -> Vec<u8>,
{
    let mut out = (items as u32).to_le_bytes().to_vec();
    for i in 0..items {
        out.extend_from_slice(&encode_item(i));
    }
    out
}

pub fn borsh_array<F>(items: usize, mut encode_item: F) -> Vec<u8>
where
    F: FnMut(usize) -> Vec<u8>,
{
    let mut out = Vec::new();
    for i in 0..items {
        out.extend_from_slice(&encode_item(i));
    }
    out
}
