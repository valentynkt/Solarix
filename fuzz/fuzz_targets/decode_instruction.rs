// Fuzz target for the Solarix instruction decoder.
//
// Goal: prove that `ChainparserDecoder::decode_instruction` does NOT panic for
// any input of length 0..=4096 bytes when called with a known-good IDL.
// libfuzzer catches panics for us — there is no need for `catch_unwind`.
//
// The IDL is bundled at compile time so the fuzzer does not need any
// filesystem access. We use the same `simple_v030.json` fixture that the
// integration tests use, so the decoder is exercised against a real Anchor
// IDL shape (instructions, accounts, types, discriminators).

#![no_main]

use libfuzzer_sys::fuzz_target;

use solarix::decoder::{ChainparserDecoder, SolarixDecoder};
use std::sync::OnceLock;

const IDL_JSON: &str = include_str!("../../tests/fixtures/idls/simple_v030.json");

fn idl() -> &'static anchor_lang_idl_spec::Idl {
    static IDL: OnceLock<anchor_lang_idl_spec::Idl> = OnceLock::new();
    IDL.get_or_init(|| serde_json::from_str(IDL_JSON).expect("bundled fixture IDL must parse"))
}

fn decoder() -> &'static ChainparserDecoder {
    static DEC: OnceLock<ChainparserDecoder> = OnceLock::new();
    DEC.get_or_init(ChainparserDecoder::new)
}

fuzz_target!(|data: &[u8]| {
    // Bound the input — AC3 specifies inputs up to 4096 bytes.
    if data.len() > 4096 {
        return;
    }
    // Result is intentionally discarded; we only care that decoding does not
    // panic. Errors (UnknownDiscriminator, DeserializationFailed, etc.) are
    // expected and acceptable for arbitrary fuzz input.
    let _ = decoder().decode_instruction("fuzz_program", data, idl());
});
