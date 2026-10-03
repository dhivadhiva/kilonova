//! The API surface the Kilonova app calls through `flutter_rust_bridge`.
//!
//! Everything under [`api`] is exported to Dart. Keep this crate thin: it
//! converts between FFI-friendly types and the wallet crates, and holds no
//! wallet logic of its own.

pub mod api;
#[cfg(test)]
mod bench;
mod node_settings;
#[cfg(test)]
mod test_store;

// Generated and formatted by flutter_rust_bridge_codegen; CI checks it is
// current, so rustfmt must leave it as generated.
#[allow(clippy::all, clippy::pedantic, unsafe_code)]
#[rustfmt::skip]
mod frb_generated;
