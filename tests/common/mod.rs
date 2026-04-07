// Shared helpers for Solarix integration tests.
//
// Cargo treats every file in `tests/` as its own crate, so any helper that
// needs to be reused across multiple `tests/*.rs` files must live in
// `tests/common/mod.rs` (the canonical Cargo "no, this is not a test target"
// convention) or be included via `#[path = ...] mod common;`.
//
// Some sub-modules are imported by only a subset of integration test crates;
// `#[allow(dead_code)]` suppresses the per-crate "unused" lint that fires when
// a particular test file does not pull in every helper.

#![allow(dead_code)]

pub mod decoder_fixtures;
pub mod known_programs;

// `tests/common/postgres.rs` carries `#![cfg(feature = "integration")]` at
// the top of the file itself, so a redundant `#[cfg(...)]` here would trip
// `clippy::duplicated_attributes`. The bare `pub mod` declaration is enough
// — the file-level cfg gates the contents.
pub mod postgres;
