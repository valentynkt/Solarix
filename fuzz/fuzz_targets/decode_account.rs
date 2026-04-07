// Fuzz target for the Solarix account decoder.
//
// Same shape as decode_instruction.rs but exercises the account discriminator
// path. AC3 demands "no panic" for inputs of length 0..=4096 bytes against the
// bundled fixture IDL.

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
    if data.len() > 4096 {
        return;
    }
    let _ = decoder().decode_account("fuzz_program", "fuzz_pubkey", data, idl());
});
