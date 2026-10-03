//! Coin control: the wallet's unspent outputs ("coins"), freezing ones the
//! owner does not want spent, and choosing which to spend.
//!
//! Arguments are owned because `flutter_rust_bridge` hands them over that way.
#![allow(clippy::needless_pass_by_value)]

use std::sync::atomic::Ordering;

use flutter_rust_bridge::frb;

use super::sync::hex_string;
use super::wallets::{OpenWallet, WalletError, store};

/// One unspent output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoinRow {
    /// One-time output key (hex): the coin's identity.
    pub key: String,
    /// Atomic units.
    pub amount: u64,
    /// Block it arrived in.
    pub height: u64,
    /// Subaddress index in account 0 it arrived on (0 is the primary).
    pub subaddress_index: u32,
    pub miner: bool,
    /// Cannot be spent yet.
    pub locked: bool,
    /// The owner froze it: never spent until thawed.
    pub frozen: bool,
    /// The transaction it arrived in (hex).
    pub tx_hash: String,
}

impl OpenWallet {
    /// Unspent coins, largest first.
    ///
    /// # Errors
    ///
    /// Fails if the wallet has been locked.
    #[frb(sync)]
    pub fn coins(&self) -> Result<Vec<CoinRow>, WalletError> {
        let frozen = self.inner.with(|w| Ok(w.data.frozen.clone()))?;
        let tip = self.inner.sync.tip.load(Ordering::Relaxed);
        let mut rows: Vec<CoinRow> = self.inner.sync.read(|state| {
            state
                .outputs
                .iter()
                .filter(|o| o.spent.is_none())
                .map(|o| {
                    let key = hex_string(&o.output.key().compress().to_bytes());
                    CoinRow {
                        frozen: frozen.contains(&key),
                        key,
                        amount: o.amount(),
                        height: o.height,
                        subaddress_index: o.output.subaddress().map_or(0, |s| s.address()),
                        miner: o.miner,
                        locked: tip < o.unlock_height(),
                        tx_hash: hex_string(&o.output.transaction()),
                    }
                })
                .collect()
        });
        rows.sort_by_key(|r| std::cmp::Reverse(r.amount));
        Ok(rows)
    }

    /// Freezes or thaws the coin with one-time key `key` (hex).
    ///
    /// # Errors
    ///
    /// Fails if the wallet has been locked or cannot be saved.
    pub fn set_coin_frozen(&self, key: String, frozen: bool) -> Result<(), WalletError> {
        self.inner.with(|w| {
            if frozen {
                w.data.frozen.insert(key);
            } else {
                w.data.frozen.remove(&key);
            }
            Ok(store()?.save(w)?)
        })
    }
}
