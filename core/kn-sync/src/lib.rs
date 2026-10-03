//! Full-mode sync: Kilonova scans the chain itself against a monerod node.
//!
//! The node sees which blocks are fetched, never which outputs are the
//! wallet's: scanning and key-image checks happen here, with keys from
//! `kn-keys`. Uses monero-oxide's daemon client and scanner.

#![forbid(unsafe_code)]

mod blocks;
mod crosscheck;
mod discover;
mod lws;
mod node;
mod opinion;
mod price;
mod proof;
mod restore_height;
mod scan;
mod state;
mod tls;

pub use crosscheck::{CROSS_CHECK_LIMIT, CrossCheck, cross_check};
pub use discover::{FoundNode, discover, discover_at, local_ipv4};
#[cfg(feature = "fuzzing")]
pub use lws::fuzzing;
pub use lws::{LwsFees, LwsInfo, LwsReport, LwsServer, RandomOutput, check_lws, lws_sync};
pub use node::{
    Http, NodeStatus, NodeUrl, ProxyUrl, ServerPairing, bundled_nodes, check_proxy, connect, proxy,
    set_proxy,
};
/// The node client sync and sending use.
pub type MoneroDaemonHttp = monero_daemon_rpc::MoneroDaemon<Http>;
pub use opinion::{OPINION_DEPTH, Opinion, second_opinion};
pub use price::{PRICE_CURRENCIES, PRICE_SOURCE, xmr_price};
pub use proof::{ProofError, ProofResult, check_tx_key};
pub use restore_height::approximate_height;
pub use scan::{Progress, SUBADDRESS_LOOKAHEAD, SyncCache, sync, sync_with};
pub use state::{
    Balance, DEFAULT_LOCK_BLOCKS, Direction, HistoryEntry, MINER_LOCK_BLOCKS, OwnedOutput,
    PENDING_EXPIRY_BLOCKS, PoolPayment, Spend, SyncState,
};
pub use tls::{CertificateInfo, Fingerprint, fingerprint, server_certificate, set_pins};

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("not a valid node address; use host:port or http(s)://host:port")]
    BadNodeUrl,
    #[error("this node serves a different Monero network")]
    WrongNetwork,
    #[error("node error: {0}")]
    Node(String),
    #[error("sync was cancelled")]
    Cancelled,
    #[error("the light wallet server refused this wallet")]
    LwsDenied,
    #[error("the light wallet server does not accept new wallets")]
    LwsCreationRefused,
    #[error("not a valid proxy; use host:port or socks5h://host:port")]
    BadProxyUrl,
    #[error("onion addresses need a Tor proxy")]
    NeedsProxy,
    #[error(
        "a light wallet server receives the view key, so it must use https (or be an onion or local address)"
    )]
    InsecureLws,
}

impl From<monero_interface::InterfaceError> for SyncError {
    fn from(e: monero_interface::InterfaceError) -> Self {
        Self::Node(e.to_string())
    }
}
