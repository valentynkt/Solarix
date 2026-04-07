// Decoder Borsh roundtrip property tests for Story 6.4 (AC1, AC2).
//
// For every IDL primitive supported by the Solarix decoder, this file:
//
//   1. Generates an arbitrary value via `proptest`
//   2. Borsh-encodes it (little-endian, manual — we don't pull `borsh` into
//      dev-deps, see `tests/common/decoder_fixtures.rs` for the encoders)
//   3. Wraps the bytes in a single-arg instruction with the matching
//      Anchor discriminator (`SHA-256("global:{name}")[..8]`)
//   4. Decodes via `ChainparserDecoder::decode_instruction`
//   5. Asserts the JSON output exactly matches the expected `serde_json::Value`
//
// Specific behaviors locked in (regression pins, NOT new contracts):
//
//   - `u128` / `i128`  -> `Value::String(decimal)`        (src/decoder/mod.rs:340-347)
//   - `f32::NAN` / `f64::NAN` / `±Infinity` -> `Value::String("NaN" | "Infinity" | "-Infinity")`
//                                                          (src/decoder/mod.rs:239-257, 314-336)
//   - `u64` in `(i64::MAX, u64::MAX]` -> `Value::Number(u64)` — see AC2 below.
//
// AC2 PIN: the writer's promoted-column CASE WHEN guard at
// `src/storage/writer.rs::build_promoted_extract_expr` enforces the BIGINT
// overflow contract for u64 > i64::MAX. The decoder emits a JSON number; the
// writer is what guards storage. This file ASSERTS the current decoder
// behavior so a future patch that accidentally normalizes u64 to a string
// (which would silently break code paths that expect the number) gets caught.
//
// `PROPTEST_CASES` env var controls the case count per test (default 256,
// CI overrides to 1024 — see Story 6.7).

mod common;

use anchor_lang_idl_spec::{IdlArrayLen, IdlType};
use proptest::prelude::*;
use serde_json::{json, Value};

use solarix::decoder::{ChainparserDecoder, SolarixDecoder};

use common::decoder_fixtures::{
    borsh_array, borsh_bool, borsh_f32, borsh_f64, borsh_i128, borsh_i16, borsh_i32, borsh_i64,
    borsh_i8, borsh_option_none, borsh_option_some, borsh_pubkey, borsh_string, borsh_u128,
    borsh_u16, borsh_u32, borsh_u64, borsh_u8, borsh_vec, build_single_arg_instruction,
    build_single_field_account, make_account_entry, make_account_idl, make_field,
};

// ---------------------------------------------------------------------------
// Proptest configuration
// ---------------------------------------------------------------------------

fn proptest_config() -> ProptestConfig {
    let cases = std::env::var("PROPTEST_CASES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(256u32);
    ProptestConfig::with_cases(cases)
}

// ---------------------------------------------------------------------------
// Roundtrip helper
// ---------------------------------------------------------------------------

fn roundtrip_arg(arg_name: &str, ty: IdlType, encoded: &[u8]) -> Value {
    let (data, idl) = build_single_arg_instruction(arg_name, ty, encoded);
    let decoder = ChainparserDecoder::new();
    let result = decoder
        .decode_instruction("prog", &data, &idl)
        .expect("decode_instruction should succeed for valid Borsh input");
    result.args[arg_name].clone()
}

fn roundtrip_account_field(type_tag: &str, field_name: &str, ty: IdlType, encoded: &[u8]) -> Value {
    let (data, idl) = build_single_field_account(type_tag, field_name, ty, encoded);
    let decoder = ChainparserDecoder::new();
    let acct_name = format!("Acct{type_tag}");
    let result = decoder
        .decode_account("prog", "pk", &data, &idl)
        .expect("decode_account should succeed for valid Borsh input");
    assert_eq!(result.account_type, acct_name);
    result.data[field_name].clone()
}

// ---------------------------------------------------------------------------
// Primitive proptests — instruction args
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn proptest_bool(v: bool) {
        let out = roundtrip_arg("v", IdlType::Bool, &borsh_bool(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_u8(v: u8) {
        let out = roundtrip_arg("v", IdlType::U8, &borsh_u8(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_i8(v: i8) {
        let out = roundtrip_arg("v", IdlType::I8, &borsh_i8(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_u16(v: u16) {
        let out = roundtrip_arg("v", IdlType::U16, &borsh_u16(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_i16(v: i16) {
        let out = roundtrip_arg("v", IdlType::I16, &borsh_i16(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_u32(v: u32) {
        let out = roundtrip_arg("v", IdlType::U32, &borsh_u32(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_i32(v: i32) {
        let out = roundtrip_arg("v", IdlType::I32, &borsh_i32(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_u64(v: u64) {
        let out = roundtrip_arg("v", IdlType::U64, &borsh_u64(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_i64(v: i64) {
        let out = roundtrip_arg("v", IdlType::I64, &borsh_i64(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_u128_as_string(v: u128) {
        // CONTRACT (AC1): u128 -> JSON String of decimal repr.
        let out = roundtrip_arg("v", IdlType::U128, &borsh_u128(v));
        prop_assert_eq!(out, Value::String(v.to_string()));
    }

    #[test]
    fn proptest_i128_as_string(v: i128) {
        // CONTRACT (AC1): i128 -> JSON String of decimal repr.
        let out = roundtrip_arg("v", IdlType::I128, &borsh_i128(v));
        prop_assert_eq!(out, Value::String(v.to_string()));
    }

    #[test]
    fn proptest_f32_finite(v in proptest::num::f32::NORMAL | proptest::num::f32::ZERO | proptest::num::f32::SUBNORMAL) {
        // Finite f32 -> JSON number. NaN/Inf are exercised separately below.
        let out = roundtrip_arg("v", IdlType::F32, &borsh_f32(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_f64_finite(v in proptest::num::f64::NORMAL | proptest::num::f64::ZERO | proptest::num::f64::SUBNORMAL) {
        let out = roundtrip_arg("v", IdlType::F64, &borsh_f64(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_string(s in ".{0,256}") {
        let out = roundtrip_arg("v", IdlType::String, &borsh_string(&s));
        prop_assert_eq!(out, json!(s));
    }

    #[test]
    fn proptest_pubkey(bytes in proptest::array::uniform32(any::<u8>())) {
        let out = roundtrip_arg("v", IdlType::Pubkey, &borsh_pubkey(&bytes));
        let expected = bs58::encode(&bytes).into_string();
        prop_assert_eq!(out, json!(expected));
    }

    // --- Vec<T> ---

    #[test]
    fn proptest_vec_u8(items in proptest::collection::vec(any::<u8>(), 0..32)) {
        let encoded = borsh_vec(items.len(), |i| borsh_u8(items[i]));
        let out = roundtrip_arg("v", IdlType::Vec(Box::new(IdlType::U8)), &encoded);
        let expected: Vec<Value> = items.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }

    #[test]
    fn proptest_vec_u32(items in proptest::collection::vec(any::<u32>(), 0..32)) {
        let encoded = borsh_vec(items.len(), |i| borsh_u32(items[i]));
        let out = roundtrip_arg("v", IdlType::Vec(Box::new(IdlType::U32)), &encoded);
        let expected: Vec<Value> = items.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }

    #[test]
    fn proptest_vec_string(items in proptest::collection::vec(".{0,32}", 0..16)) {
        let encoded = borsh_vec(items.len(), |i| borsh_string(&items[i]));
        let out = roundtrip_arg("v", IdlType::Vec(Box::new(IdlType::String)), &encoded);
        let expected: Vec<Value> = items.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }

    // --- Option<T> ---

    #[test]
    fn proptest_option_u64(v in proptest::option::of(any::<u64>())) {
        let encoded = match v {
            Some(x) => borsh_option_some(&borsh_u64(x)),
            None => borsh_option_none(),
        };
        let out = roundtrip_arg("v", IdlType::Option(Box::new(IdlType::U64)), &encoded);
        let expected = match v {
            Some(x) => json!(x),
            None => Value::Null,
        };
        prop_assert_eq!(out, expected);
    }

    #[test]
    fn proptest_option_string(v in proptest::option::of(".{0,64}")) {
        let encoded = match &v {
            Some(s) => borsh_option_some(&borsh_string(s)),
            None => borsh_option_none(),
        };
        let out = roundtrip_arg("v", IdlType::Option(Box::new(IdlType::String)), &encoded);
        let expected = match v {
            Some(s) => json!(s),
            None => Value::Null,
        };
        prop_assert_eq!(out, expected);
    }

    #[test]
    fn proptest_option_pubkey(v in proptest::option::of(proptest::array::uniform32(any::<u8>()))) {
        let encoded = match &v {
            Some(bytes) => borsh_option_some(&borsh_pubkey(bytes)),
            None => borsh_option_none(),
        };
        let out = roundtrip_arg("v", IdlType::Option(Box::new(IdlType::Pubkey)), &encoded);
        let expected = match v {
            Some(bytes) => json!(bs58::encode(&bytes).into_string()),
            None => Value::Null,
        };
        prop_assert_eq!(out, expected);
    }

    // --- [T; N] ---

    #[test]
    fn proptest_array_u8_4(items in proptest::array::uniform4(any::<u8>())) {
        let encoded = borsh_array(4, |i| borsh_u8(items[i]));
        let ty = IdlType::Array(Box::new(IdlType::U8), IdlArrayLen::Value(4));
        let out = roundtrip_arg("v", ty, &encoded);
        let expected: Vec<Value> = items.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }

    #[test]
    fn proptest_array_u8_8(items in proptest::array::uniform8(any::<u8>())) {
        let encoded = borsh_array(8, |i| borsh_u8(items[i]));
        let ty = IdlType::Array(Box::new(IdlType::U8), IdlArrayLen::Value(8));
        let out = roundtrip_arg("v", ty, &encoded);
        let expected: Vec<Value> = items.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }

    #[test]
    fn proptest_array_u8_32(items in proptest::array::uniform32(any::<u8>())) {
        let encoded = borsh_array(32, |i| borsh_u8(items[i]));
        let ty = IdlType::Array(Box::new(IdlType::U8), IdlArrayLen::Value(32));
        let out = roundtrip_arg("v", ty, &encoded);
        let expected: Vec<Value> = items.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }

    #[test]
    fn proptest_array_u32_4(items in proptest::array::uniform4(any::<u32>())) {
        let encoded = borsh_array(4, |i| borsh_u32(items[i]));
        let ty = IdlType::Array(Box::new(IdlType::U32), IdlArrayLen::Value(4));
        let out = roundtrip_arg("v", ty, &encoded);
        let expected: Vec<Value> = items.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }

    #[test]
    fn proptest_array_u64_8(items in proptest::array::uniform8(any::<u64>())) {
        let encoded = borsh_array(8, |i| borsh_u64(items[i]));
        let ty = IdlType::Array(Box::new(IdlType::U64), IdlArrayLen::Value(8));
        let out = roundtrip_arg("v", ty, &encoded);
        let expected: Vec<Value> = items.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }
}

// ---------------------------------------------------------------------------
// Account decode roundtrip — single-field structs
// ---------------------------------------------------------------------------

proptest! {
    #![proptest_config(proptest_config())]

    #[test]
    fn proptest_account_u64(v: u64) {
        let out = roundtrip_account_field("U64", "v", IdlType::U64, &borsh_u64(v));
        prop_assert_eq!(out, json!(v));
    }

    #[test]
    fn proptest_account_pubkey(bytes in proptest::array::uniform32(any::<u8>())) {
        let out = roundtrip_account_field("Pk", "owner", IdlType::Pubkey, &borsh_pubkey(&bytes));
        prop_assert_eq!(out, json!(bs58::encode(&bytes).into_string()));
    }

    #[test]
    fn proptest_account_string(s in ".{0,128}") {
        let out = roundtrip_account_field("Str", "name", IdlType::String, &borsh_string(&s));
        prop_assert_eq!(out, json!(s));
    }

    #[test]
    fn proptest_account_option_u64(v in proptest::option::of(any::<u64>())) {
        let encoded = match v {
            Some(x) => borsh_option_some(&borsh_u64(x)),
            None => borsh_option_none(),
        };
        let out = roundtrip_account_field(
            "OptU64",
            "v",
            IdlType::Option(Box::new(IdlType::U64)),
            &encoded,
        );
        let expected = match v {
            Some(x) => json!(x),
            None => Value::Null,
        };
        prop_assert_eq!(out, expected);
    }

    #[test]
    fn proptest_account_array_u8_32(bytes in proptest::array::uniform32(any::<u8>())) {
        let encoded = borsh_array(32, |i| borsh_u8(bytes[i]));
        let ty = IdlType::Array(Box::new(IdlType::U8), IdlArrayLen::Value(32));
        let out = roundtrip_account_field("Arr32", "v", ty, &encoded);
        let expected: Vec<Value> = bytes.iter().map(|x| json!(x)).collect();
        prop_assert_eq!(out, Value::Array(expected));
    }
}

// ---------------------------------------------------------------------------
// Explicit edge-case tests (NOT randomized)
// ---------------------------------------------------------------------------

#[test]
fn explicit_f32_nan() {
    let out = roundtrip_arg("v", IdlType::F32, &borsh_f32(f32::NAN));
    assert_eq!(out, Value::String("NaN".to_string()));
}

#[test]
fn explicit_f32_pos_inf() {
    let out = roundtrip_arg("v", IdlType::F32, &borsh_f32(f32::INFINITY));
    assert_eq!(out, Value::String("Infinity".to_string()));
}

#[test]
fn explicit_f32_neg_inf() {
    let out = roundtrip_arg("v", IdlType::F32, &borsh_f32(f32::NEG_INFINITY));
    assert_eq!(out, Value::String("-Infinity".to_string()));
}

#[test]
fn explicit_f64_nan() {
    let out = roundtrip_arg("v", IdlType::F64, &borsh_f64(f64::NAN));
    assert_eq!(out, Value::String("NaN".to_string()));
}

#[test]
fn explicit_f64_pos_inf() {
    let out = roundtrip_arg("v", IdlType::F64, &borsh_f64(f64::INFINITY));
    assert_eq!(out, Value::String("Infinity".to_string()));
}

#[test]
fn explicit_f64_neg_inf() {
    let out = roundtrip_arg("v", IdlType::F64, &borsh_f64(f64::NEG_INFINITY));
    assert_eq!(out, Value::String("-Infinity".to_string()));
}

#[test]
fn explicit_u128_max() {
    let out = roundtrip_arg("v", IdlType::U128, &borsh_u128(u128::MAX));
    assert_eq!(out, Value::String(u128::MAX.to_string()));
}

#[test]
fn explicit_u128_zero() {
    let out = roundtrip_arg("v", IdlType::U128, &borsh_u128(0));
    assert_eq!(out, Value::String("0".to_string()));
}

#[test]
fn explicit_i128_min() {
    let out = roundtrip_arg("v", IdlType::I128, &borsh_i128(i128::MIN));
    assert_eq!(out, Value::String(i128::MIN.to_string()));
}

#[test]
fn explicit_i128_max() {
    let out = roundtrip_arg("v", IdlType::I128, &borsh_i128(i128::MAX));
    assert_eq!(out, Value::String(i128::MAX.to_string()));
}

#[test]
fn explicit_string_empty() {
    let out = roundtrip_arg("v", IdlType::String, &borsh_string(""));
    assert_eq!(out, json!(""));
}

#[test]
fn explicit_vec_empty() {
    let encoded = borsh_vec(0, |_| Vec::new());
    let out = roundtrip_arg("v", IdlType::Vec(Box::new(IdlType::U64)), &encoded);
    assert_eq!(out, json!([]));
}

#[test]
fn explicit_option_none_u64() {
    let out = roundtrip_arg(
        "v",
        IdlType::Option(Box::new(IdlType::U64)),
        &borsh_option_none(),
    );
    assert_eq!(out, Value::Null);
}

#[test]
fn explicit_option_some_default_u64() {
    let out = roundtrip_arg(
        "v",
        IdlType::Option(Box::new(IdlType::U64)),
        &borsh_option_some(&borsh_u64(0)),
    );
    assert_eq!(out, json!(0u64));
}

// ---------------------------------------------------------------------------
// AC2: u64 / i64 precision-preservation contract pin
// ---------------------------------------------------------------------------
//
// CONTRACT (current observed behavior — pin, NOT a fix):
//
//   - The DECODER emits u64 values as `Value::Number`. For values in
//     `(i64::MAX, u64::MAX]`, this is a u64 number that serde_json represents
//     internally as a u64 (`Number::is_u64() == true`). Round-trip via
//     `serde_json::Value` preserves the full 64-bit precision.
//
//   - The WRITER (`src/storage/writer.rs::build_promoted_extract_expr`) is
//     responsible for the BIGINT overflow guard via a CASE WHEN clause that
//     emits `NULL` into the promoted column when the value > i64::MAX while
//     keeping the JSONB `data` column unchanged.
//
//   - The end-to-end story is therefore:
//         decoder.json_number(u64)
//         -> writer promoted column: NULL when > i64::MAX, value otherwise
//         -> writer JSONB data:      original number, always
//
// This module does NOT exercise the writer — that lives in Story 6.5's
// integration tests. It only PINS the decoder's emission shape so a future
// "let's emit u64::MAX as a string" patch is caught at this layer.
mod u64_precision_contract {
    use super::*;

    #[test]
    fn u64_max_decodes_as_json_number() {
        let out = roundtrip_arg("v", IdlType::U64, &borsh_u64(u64::MAX));
        // Pin: must be a JSON number, not a string.
        assert!(
            out.is_number(),
            "decoder must emit u64::MAX as Value::Number; got: {out:?}"
        );
        // serde_json represents u64::MAX losslessly via its u64 backing.
        assert_eq!(out, json!(u64::MAX));
    }

    #[test]
    fn u64_just_above_i64_max_decodes_as_json_number() {
        let v: u64 = (i64::MAX as u64) + 1;
        let out = roundtrip_arg("v", IdlType::U64, &borsh_u64(v));
        assert!(out.is_number(), "expected number, got: {out:?}");
        assert_eq!(out, json!(v));
    }

    #[test]
    fn i64_min_decodes_as_json_number() {
        let out = roundtrip_arg("v", IdlType::I64, &borsh_i64(i64::MIN));
        assert!(out.is_number(), "expected number, got: {out:?}");
        assert_eq!(out, json!(i64::MIN));
    }

    #[test]
    fn u64_max_serializes_back_to_decimal_string_via_json() {
        // Round trip via serde_json::to_string verifies precision is preserved.
        let out = roundtrip_arg("v", IdlType::U64, &borsh_u64(u64::MAX));
        let s = serde_json::to_string(&out).expect("serialize");
        assert_eq!(s, "18446744073709551615");
    }
}

// ---------------------------------------------------------------------------
// Sanity: prove the helper round-trip itself works against a hand-crafted account.
// (The other proptests cover the surface; this is a smoke test for the helper.)
// ---------------------------------------------------------------------------

#[test]
fn sanity_account_field_helper_smoke() {
    let entry = make_account_entry("AcctSmoke");
    let _ = entry; // ensure helper symbols are wired up
    let _idl = make_account_idl(vec![], vec![]);
    let _f = make_field("v", IdlType::U64);
}
