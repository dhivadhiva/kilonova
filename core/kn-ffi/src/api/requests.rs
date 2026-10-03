//! Payment requests: "send me 0.25 XMR" with a subaddress made for it, so
//! the wallet can tell when that payment arrives.
//!
//! Arguments are owned because `flutter_rust_bridge` hands them over that way.
#![allow(clippy::needless_pass_by_value)]

use std::time::{SystemTime, UNIX_EPOCH};

use flutter_rust_bridge::frb;
use kn_store::PaymentRequest;
use rand_core::{OsRng, RngCore};

use super::wallets::{OpenWallet, WalletError, store};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RequestStatus {
    /// Nothing received yet.
    Waiting,
    /// A payment is in the pool, not yet in a block.
    Arriving,
    /// Less than asked was received.
    Partial,
    /// At least the amount asked was received.
    Paid,
    /// Expired before being paid.
    Expired,
}

/// One payment request and how it stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestRow {
    pub id: String,
    /// The subaddress made for it.
    pub address: String,
    /// A `monero:` link with the amount and label, for a QR code.
    pub uri: String,
    /// Atomic units asked for.
    pub amount: u64,
    pub label: String,
    pub created_at: u64,
    pub expires_at: Option<u64>,
    /// Received in blocks so far.
    pub received: u64,
    /// Waiting in the pool.
    pub arriving: u64,
    pub status: RequestStatus,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

/// `1.5` for 1.5 XMR: trailing zeros dropped, as in `monero:` links.
fn decimal_xmr(atomic: u64) -> String {
    let whole = atomic / 1_000_000_000_000;
    let fraction = format!("{:012}", atomic % 1_000_000_000_000);
    let fraction = fraction.trim_end_matches('0');
    if fraction.is_empty() {
        whole.to_string()
    } else {
        format!("{whole}.{fraction}")
    }
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(b).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

impl OpenWallet {
    /// Creates a request for `amount` (atomic units) on a new subaddress
    /// labelled `label`, optionally expiring after `expires_in_hours`.
    ///
    /// # Errors
    ///
    /// Fails for a zero amount ([`WalletError::Storage`] is not used for
    /// that: [`WalletError::EmptyName`] stands for "nothing asked"), or if
    /// the wallet cannot be saved.
    pub fn create_request(
        &self,
        amount: u64,
        label: String,
        expires_in_hours: Option<u32>,
    ) -> Result<RequestRow, WalletError> {
        if amount == 0 {
            return Err(WalletError::EmptyName);
        }
        let label = label.trim().to_owned();
        let row = self.new_address(label.clone())?;
        let created_at = now();
        let mut id = [0u8; 8];
        OsRng.fill_bytes(&mut id);
        let request = PaymentRequest {
            id: hex::encode(id),
            index: row.index,
            amount,
            label,
            created_at,
            expires_at: expires_in_hours.map(|h| created_at + u64::from(h) * 3600),
        };
        self.inner.with(|w| {
            w.data.requests.push(request.clone());
            Ok(store()?.save(w)?)
        })?;
        Ok(self.request_row(&request, &row.address))
    }

    /// Requests, newest first, with what each has received.
    ///
    /// # Errors
    ///
    /// Fails if the wallet has been locked.
    #[frb(sync)]
    pub fn requests(&self) -> Result<Vec<RequestRow>, WalletError> {
        let requests = self.inner.with(|w| Ok(w.data.requests.clone()))?;
        let mut rows = Vec::with_capacity(requests.len());
        for r in requests.iter().rev() {
            let address = self.inner.with(|w| {
                w.keys
                    .address(w.entry.network, 0, r.index)
                    .map_err(WalletError::from)
            })?;
            rows.push(self.request_row(r, &address));
        }
        Ok(rows)
    }

    /// Removes a request. Its subaddress stays, with its label.
    ///
    /// # Errors
    ///
    /// Fails if the wallet has been locked or cannot be saved.
    pub fn delete_request(&self, id: String) -> Result<(), WalletError> {
        self.inner.with(|w| {
            w.data.requests.retain(|r| r.id != id);
            Ok(store()?.save(w)?)
        })
    }

    fn request_row(&self, r: &PaymentRequest, address: &str) -> RequestRow {
        let (received, arriving) = self.inner.sync.read(|s| s.received_by(0, r.index));
        let status = if received >= r.amount {
            RequestStatus::Paid
        } else if arriving > 0 {
            RequestStatus::Arriving
        } else if received > 0 {
            RequestStatus::Partial
        } else if r.expires_at.is_some_and(|t| now() > t) {
            RequestStatus::Expired
        } else {
            RequestStatus::Waiting
        };
        let mut uri = format!("monero:{address}?tx_amount={}", decimal_xmr(r.amount));
        if !r.label.is_empty() {
            uri.push_str("&tx_description=");
            uri.push_str(&percent_encode(&r.label));
        }
        RequestRow {
            id: r.id.clone(),
            address: address.to_owned(),
            uri,
            amount: r.amount,
            label: r.label.clone(),
            created_at: r.created_at,
            expires_at: r.expires_at,
            received,
            arriving,
            status,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::network::Network;
    use super::super::wallets::{SeedFormat, SyncMode, create_wallet_from_seed, generate_seed};
    use super::*;

    #[test]
    fn requests_get_their_own_subaddress_and_a_link() {
        crate::test_store::init();
        let wallet = create_wallet_from_seed(
            "Shop".into(),
            Network::Stagenet,
            SyncMode::Full,
            generate_seed(SeedFormat::Classic).words.join(" "),
            "pw".into(),
            Some(5),
            true,
        )
        .unwrap();
        assert_eq!(
            wallet
                .create_request(0, "nothing".into(), None)
                .unwrap_err(),
            WalletError::EmptyName
        );
        let first = wallet
            .create_request(250_000_000_000, "Coffee".into(), Some(24))
            .unwrap();
        let second = wallet
            .create_request(1_000_000_000_000, String::new(), None)
            .unwrap();
        assert_ne!(first.address, second.address);
        assert_eq!(first.status, RequestStatus::Waiting);
        assert_eq!(
            first.uri,
            format!(
                "monero:{}?tx_amount=0.25&tx_description=Coffee",
                first.address
            )
        );
        assert_eq!(second.uri, format!("monero:{}?tx_amount=1", second.address));
        assert!(first.expires_at.unwrap() > first.created_at);
        // Newest first; the subaddress keeps the request's label.
        let rows = wallet.requests().unwrap();
        assert_eq!(rows[0].id, second.id);
        assert!(
            wallet
                .addresses()
                .unwrap()
                .iter()
                .any(|a| a.address == first.address && a.label == "Coffee")
        );
        wallet.delete_request(first.id.clone()).unwrap();
        assert_eq!(wallet.requests().unwrap().len(), 1);
    }

    #[test]
    fn amounts_and_labels_are_written_for_links() {
        assert_eq!(decimal_xmr(1_500_000_000_000), "1.5");
        assert_eq!(decimal_xmr(2_000_000_000_000), "2");
        assert_eq!(decimal_xmr(1), "0.000000000001");
        assert_eq!(percent_encode("Rent, May"), "Rent%2C%20May");
    }
}
