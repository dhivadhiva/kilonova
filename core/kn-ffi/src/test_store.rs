//! One wallet store for every test in this crate: the store is
//! process-global, so tests must share it and only look at their own data.

use std::sync::OnceLock;

static DIR: OnceLock<tempfile::TempDir> = OnceLock::new();

pub(crate) fn init() {
    let dir = DIR.get_or_init(|| tempfile::tempdir().expect("temp dir"));
    crate::api::wallets::init_wallet_store(dir.path().to_string_lossy().into_owned())
        .expect("store opens");
}

#[cfg(test)]
pub(crate) fn dir() -> std::path::PathBuf {
    DIR.get().expect("store initialized").path().to_path_buf()
}
