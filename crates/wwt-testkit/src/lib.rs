//! Test harness for the translator.
//!
//! * Layer 1 (instructions): [`gen`] builds cases for every instruction
//!   form ([`gen64`] for x86-64), [`oracle`] runs them on the real CPU, [`exec`] runs them through
//!   the translator and wasmtime, [`compare`] checks the results.
//! * Fixtures recorded on x86 hardware live in `tests/fixtures/instructions`
//!   so the suite runs anywhere.

pub mod case;
pub mod compare;
pub mod exec;
pub mod fpucmp;
pub mod gen;
pub mod gen64;
pub mod layout;
pub mod oracle;
pub mod suite;
