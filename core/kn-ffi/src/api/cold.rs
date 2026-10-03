//! Offline signing for the app: a cold wallet (full keys, never online)
//! signs for a watching wallet (view-only, syncs). Messages move as
//! animated QR codes or files; see `kn_tx::cold` for what each carries and
//! what the cold wallet checks before signing.
//!
//! Arguments are owned because `flutter_rust_bridge` hands them over that way.
#![allow(clippy::needless_pass_by_value)]

use std::sync::Mutex;

use flutter_rust_bridge::frb;
use kn_tx::cold::{self, Assembler, ColdError, Envelope, FRAME_CHARS, Kind};
use kn_tx::{Priority, Request};
use zeroize::Zeroizing;

use super::nodes::RUNTIME;
use super::send::{FeePriority, Payment, Route, SendError, SendSummary};
use super::sync::hex_string;
use super::wallets::{OpenWallet, SyncMode, WalletError, create_view_only_wallet, store};

/// Why a cold wallet step failed. The app maps each case to its own message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdFailure {
    /// Not a Kilonova cold wallet code or file.
    NotColdData,
    /// Meant for another wallet.
    WrongWallet,
    /// Meant for another network.
    WrongNetwork,
    /// The wrong step: for example a signed answer scanned on the cold wallet.
    WrongKind,
    /// Missing parts or damaged; scan again.
    Damaged,
    /// The transaction spends coins that are not this wallet's.
    NotOurs,
    /// The transaction's change goes somewhere else.
    ChangeElsewhere,
    BadAddress,
    FeeTooHigh,
    /// The amounts do not add up.
    Inconsistent,
    /// Only a wallet with its spend key can do this.
    ViewOnly,
    /// Only a view-only wallet does this; this one can sign itself.
    NotWatching,
    /// A cold wallet never goes online.
    IsCold,
    WrongPassword,
    /// The prepared request was already answered or discarded.
    AlreadyUsed,
    /// Building or publishing failed; the details are in [`SendError`].
    Send,
    Locked,
    Storage,
}

impl From<ColdError> for ColdFailure {
    fn from(e: ColdError) -> Self {
        match e {
            ColdError::NotColdData => Self::NotColdData,
            ColdError::WrongWallet => Self::WrongWallet,
            ColdError::WrongNetwork => Self::WrongNetwork,
            ColdError::WrongKind => Self::WrongKind,
            ColdError::Damaged => Self::Damaged,
            ColdError::NotOurs => Self::NotOurs,
            ColdError::ChangeElsewhere => Self::ChangeElsewhere,
            ColdError::BadAddress => Self::BadAddress,
            ColdError::FeeTooHigh => Self::FeeTooHigh,
            ColdError::Inconsistent | ColdError::Signing(_) => Self::Inconsistent,
            ColdError::ViewOnly => Self::ViewOnly,
        }
    }
}

impl From<WalletError> for ColdFailure {
    fn from(e: WalletError) -> Self {
        match e {
            WalletError::WrongPassword => Self::WrongPassword,
            WalletError::NotFound => Self::Locked,
            _ => Self::Storage,
        }
    }
}

/// What a message is, so the app can route a scan to the right screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColdKind {
    Pairing,
    SyncRequest,
    SyncAnswer,
    SignRequest,
    Signed,
}

impl From<Kind> for ColdKind {
    fn from(k: Kind) -> Self {
        match k {
            Kind::Pairing => Self::Pairing,
            Kind::SyncRequest => Self::SyncRequest,
            Kind::SyncAnswer => Self::SyncAnswer,
            Kind::SignRequest => Self::SignRequest,
            Kind::Signed => Self::Signed,
        }
    }
}

/// A message ready to show or save.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdMessage {
    pub kind: ColdKind,
    /// QR codes to show in turn; one when it fits.
    pub frames: Vec<String>,
    /// The same message as a file, for moving by USB or SD card.
    pub file: Vec<u8>,
}

impl ColdMessage {
    fn of(envelope: &Envelope) -> Self {
        let file = envelope.to_bytes();
        Self {
            kind: envelope.kind.into(),
            frames: cold::frames(&file, FRAME_CHARS),
            file,
        }
    }
}

/// How far a multi-part scan has come.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanProgress {
    pub received: u32,
    pub total: u32,
    pub complete: bool,
}

/// Collects scanned QR frames until a message is whole.
#[frb(opaque)]
pub struct FrameReader {
    inner: Mutex<Assembler>,
}

impl FrameReader {
    #[frb(sync)]
    #[must_use]
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(Assembler::new()),
        }
    }

    /// Adds one scanned code.
    ///
    /// # Errors
    ///
    /// [`ColdFailure::NotColdData`] for a code that is not part of a
    /// Kilonova message.
    #[frb(sync)]
    pub fn add(&self, frame: String) -> Result<ScanProgress, ColdFailure> {
        let mut assembler = self.lock();
        let progress = assembler.add(&frame)?;
        Ok(ScanProgress {
            received: u32::try_from(progress.received).unwrap_or(u32::MAX),
            total: u32::try_from(progress.total).unwrap_or(u32::MAX),
            complete: progress.received == progress.total && assembler.message().is_ok(),
        })
    }

    /// The whole message, once complete.
    ///
    /// # Errors
    ///
    /// [`ColdFailure::Damaged`] if parts are missing or do not match.
    #[frb(sync)]
    pub fn message(&self) -> Result<Vec<u8>, ColdFailure> {
        Ok(self.lock().message()?)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Assembler> {
        self.inner
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Default for FrameReader {
    fn default() -> Self {
        Self::new()
    }
}

/// What kind of message `message` (a whole scan or file) is.
///
/// # Errors
///
/// [`ColdFailure::NotColdData`] for anything else.
#[frb(sync)]
pub fn cold_message_kind(message: Vec<u8>) -> Result<ColdKind, ColdFailure> {
    Ok(Envelope::from_bytes(&message)?.kind.into())
}

/// Creates the watching wallet for a cold wallet, from its pairing message.
///
/// # Errors
///
/// [`ColdFailure::WrongKind`] for any other message, or what creating a
/// view-only wallet can fail with.
pub fn create_watching_wallet(
    name: String,
    message: Vec<u8>,
    password: String,
    mode: SyncMode,
) -> Result<OpenWallet, ColdFailure> {
    let pairing = cold::read_pairing(&Envelope::from_bytes(&message)?)?;
    Ok(create_view_only_wallet(
        name,
        pairing.network.into(),
        mode,
        pairing.address,
        pairing.view_key_hex.to_string(),
        password,
        Some(pairing.restore_height),
    )?)
}

/// What a cold wallet is asked to do, for its confirmation screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdRequestSummary {
    pub kind: ColdKind,
    /// For a sign request: who is paid what, read from the transaction.
    pub payments: Vec<Payment>,
    pub fee: u64,
    /// Coming back to this wallet.
    pub change: u64,
    /// Coins it spends.
    pub inputs: u32,
}

/// A request the cold wallet read and checked, waiting for the owner.
#[frb(opaque)]
pub struct ColdRequest {
    envelope: Envelope,
    review: Mutex<Option<cold::Review>>,
    summary: ColdRequestSummary,
}

impl std::fmt::Debug for ColdRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ColdRequest")
            .field("summary", &self.summary)
            .finish_non_exhaustive()
    }
}

impl ColdRequest {
    #[frb(sync)]
    #[must_use]
    pub fn summary(&self) -> ColdRequestSummary {
        self.summary.clone()
    }
}

/// A send the watching wallet built, waiting for the cold wallet's
/// signature.
#[frb(opaque)]
pub struct ColdSend {
    summary: SendSummary,
    message: ColdMessage,
}

impl ColdSend {
    #[frb(sync)]
    #[must_use]
    pub fn summary(&self) -> SendSummary {
        self.summary.clone()
    }

    /// The sign request to show to the cold wallet.
    #[frb(sync)]
    #[must_use]
    pub fn message(&self) -> ColdMessage {
        self.message.clone()
    }
}

/// What importing a cold wallet's answer did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ColdImport {
    /// Coins whose key images the watching wallet learned.
    pub key_images: u32,
    /// The published transaction, for a signed answer.
    pub published_tx: Option<String>,
}

impl OpenWallet {
    /// Whether this is an offline (cold) wallet.
    ///
    /// # Errors
    ///
    /// Fails if the wallet has been locked.
    #[frb(sync)]
    pub fn is_cold(&self) -> Result<bool, WalletError> {
        self.inner.with(|w| Ok(w.entry.cold))
    }

    /// Makes this an offline wallet that only signs for a watching wallet,
    /// or a normal wallet again. Stops sync and forgets what was scanned.
    ///
    /// # Errors
    ///
    /// [`ColdFailure::ViewOnly`] for a view-only wallet, which cannot sign.
    pub fn set_cold(&self, cold: bool) -> Result<(), ColdFailure> {
        self.inner.sync.stop();
        self.inner
            .with(|w| {
                if cold && w.keys.is_view_only() {
                    return Err(WalletError::InvalidKey);
                }
                store()?.set_cold(&w.entry.id, cold)?;
                w.entry.cold = cold;
                Ok(())
            })
            .map_err(|e| match e {
                WalletError::InvalidKey => ColdFailure::ViewOnly,
                other => other.into(),
            })?;
        self.inner.sync.reset(&self.inner);
        Ok(())
    }

    /// The pairing code a watching wallet is created from. It carries the
    /// private view key, so it asks for the password.
    ///
    /// # Errors
    ///
    /// [`ColdFailure::WrongPassword`], or [`ColdFailure::ViewOnly`].
    pub fn cold_pairing(&self, password: String) -> Result<ColdMessage, ColdFailure> {
        let password = Zeroizing::new(password);
        let (id, keys, network, restore) = self.inner.with(|w| {
            Ok((
                w.entry.id.clone(),
                w.keys.clone(),
                w.entry.network,
                w.data.restore_height.unwrap_or(0),
            ))
        })?;
        if keys.is_view_only() {
            return Err(ColdFailure::ViewOnly);
        }
        store()?
            .unlock(&id, password.as_bytes())
            .map_err(WalletError::from)?;
        Ok(ColdMessage::of(&cold::pairing(&keys, network, restore)))
    }

    /// Watching wallet: asks the cold wallet for the key images of every
    /// coin that lacks one, so spends become visible.
    ///
    /// # Errors
    ///
    /// [`ColdFailure::NotWatching`] for a wallet that can sign itself.
    pub fn cold_sync_request(&self) -> Result<ColdMessage, ColdFailure> {
        let (keys, network) = self.watching()?;
        let request = self
            .inner
            .sync
            .read(|state| cold::sync_request(&keys, network, state));
        Ok(ColdMessage::of(&request))
    }

    /// Watching wallet: how many coins still need key images from the cold
    /// wallet before they can be spent.
    ///
    /// # Errors
    ///
    /// Fails if the wallet has been locked.
    #[frb(sync)]
    pub fn coins_without_key_images(&self) -> Result<u32, WalletError> {
        self.inner.with(|_| Ok(()))?;
        let missing = self
            .inner
            .sync
            .read(|state| state.outputs_without_key_images().len());
        Ok(u32::try_from(missing).unwrap_or(u32::MAX))
    }

    /// Watching wallet: builds a transaction for the cold wallet to sign.
    ///
    /// # Errors
    ///
    /// [`ColdFailure::Send`] with the reason in the returned summary's
    /// absence; call [`OpenWallet::prepare_send`]'s error mapping in the app.
    pub fn prepare_cold_send(
        &self,
        payments: Vec<Payment>,
        sweep_to: Option<String>,
        priority: FeePriority,
    ) -> Result<ColdSend, SendError> {
        let (keys, network, mode, consent) = self.inner.with(|w| {
            Ok((
                w.keys.clone(),
                w.entry.network,
                w.entry.mode,
                w.data.lws_consent.clone(),
            ))
        })?;
        if !keys.is_view_only() {
            return Err(SendError::Build);
        }
        let (state, tip) = self
            .inner
            .sync
            .caught_up_state()
            .ok_or(SendError::NotSynced)?;
        let route = Route::for_wallet(mode, network, consent.as_deref())?;
        let request = match sweep_to {
            Some(address) => Request::SweepAll(address),
            None => Request::Pay(
                payments
                    .into_iter()
                    .map(|p| (p.address, p.amount))
                    .collect(),
            ),
        };
        let frozen = self
            .inner
            .with(|w| Ok(w.data.frozen.iter().cloned().collect::<Vec<_>>()))?;
        let selection = kn_tx::Selection {
            exclude: super::send::parse_keys(&frozen),
            only: None,
        };
        let unsigned = RUNTIME.block_on(route.prepare_unsigned(
            &keys,
            network,
            &state,
            tip,
            &request,
            Priority::from(priority),
            &selection,
        ))?;
        let envelope = cold::sign_request(&keys, network, &state, &unsigned);
        Ok(ColdSend {
            summary: SendSummary {
                tx_hash: String::new(),
                payments: unsigned
                    .destinations
                    .iter()
                    .map(|(address, amount)| Payment {
                        address: address.clone(),
                        amount: *amount,
                    })
                    .collect(),
                fee: unsigned.fee,
                change: unsigned.change,
                via: route.url().as_str().to_owned(),
                linked_addresses: u32::try_from(unsigned.linked_addresses).unwrap_or(u32::MAX),
            },
            message: ColdMessage::of(&envelope),
        })
    }

    /// Watching wallet: takes the cold wallet's answer, with the
    /// destinations of the [`ColdSend`] it answers (empty for a sync). Key
    /// images are
    /// learned (and the chain rescanned from the oldest affected coin in
    /// full mode); a signed transaction is checked to spend only this
    /// wallet's coins and published. Sync is paused; start it again
    /// afterwards.
    ///
    /// # Errors
    ///
    /// See [`ColdFailure`]; publishing failures are [`ColdFailure::Send`].
    pub fn import_cold_answer(
        &self,
        message: Vec<u8>,
        destinations: Vec<Payment>,
    ) -> Result<ColdImport, ColdFailure> {
        let (keys, network) = self.watching()?;
        let (mode, consent) = self
            .inner
            .with(|w| Ok((w.entry.mode, w.data.lws_consent.clone())))?;
        let answer = cold::read_answer(&keys, network, &Envelope::from_bytes(&message)?)?;

        self.inner.sync.pause();
        let mut state = self.inner.sync.snapshot();
        if let Some(height) = state.learn_key_images(&answer.key_images)
            && mode == kn_store::SyncMode::Full
        {
            state.rescan_from(height);
        }
        let mut published = None;
        if let Some(signed) = &answer.signed {
            // The rescan above may not have run yet; the spent check needs
            // key images on the coins, which learning already set.
            let route = Route::for_wallet(mode, network, consent.as_deref())
                .map_err(|_| ColdFailure::Send)?;
            let tip = state.next_height;
            let result = RUNTIME.block_on(route.publish_signed(network, signed, &mut state, tip));
            if let Err(e) = result {
                self.inner.sync.replace(&self.inner, state);
                return Err(match e {
                    SendError::Build => ColdFailure::NotOurs,
                    _ => ColdFailure::Send,
                });
            }
            let hash = hex_string(&signed.hash);
            let _ = self.inner.with(|w| {
                w.data.sent.insert(
                    hash.clone(),
                    kn_store::SentRecord {
                        destinations: destinations
                            .iter()
                            .map(|p| (p.address.clone(), p.amount))
                            .collect(),
                        tx_key: signed.tx_key.clone(),
                    },
                );
                Ok(store()?.save(w)?)
            });
            published = Some(hash);
        }
        self.inner.sync.replace(&self.inner, state);
        Ok(ColdImport {
            key_images: u32::try_from(answer.key_images.len()).unwrap_or(u32::MAX),
            published_tx: published,
        })
    }

    /// Cold wallet: reads and checks a scanned request. Nothing is signed
    /// until [`OpenWallet::answer_cold_request`].
    ///
    /// # Errors
    ///
    /// See [`ColdFailure`]: a transaction that is not this wallet's own and
    /// sane is refused here.
    pub fn read_cold_request(&self, message: Vec<u8>) -> Result<ColdRequest, ColdFailure> {
        let (keys, network) = self.signer()?;
        let envelope = Envelope::from_bytes(&message)?;
        match envelope.kind {
            Kind::SyncRequest => {
                // Checks wallet and network now, so a wrong scan fails here.
                let (_, _) = cold::answer_sync(&keys, network, &envelope)?;
                Ok(ColdRequest {
                    summary: ColdRequestSummary {
                        kind: ColdKind::SyncRequest,
                        payments: Vec::new(),
                        fee: 0,
                        change: 0,
                        inputs: 0,
                    },
                    review: Mutex::new(None),
                    envelope,
                })
            }
            Kind::SignRequest => {
                let review = cold::review(&keys, network, &envelope)?;
                Ok(ColdRequest {
                    summary: ColdRequestSummary {
                        kind: ColdKind::SignRequest,
                        payments: review
                            .destinations
                            .iter()
                            .map(|(address, amount)| Payment {
                                address: address.clone(),
                                amount: *amount,
                            })
                            .collect(),
                        fee: review.fee,
                        change: review.change,
                        inputs: u32::try_from(review.input_count).unwrap_or(u32::MAX),
                    },
                    review: Mutex::new(Some(review)),
                    envelope,
                })
            }
            _ => Err(ColdFailure::WrongKind),
        }
    }

    /// Cold wallet: answers a request. Signing asks for the password again.
    ///
    /// # Errors
    ///
    /// [`ColdFailure::WrongPassword`] leaves the request usable; a request
    /// can be signed once.
    pub fn answer_cold_request(
        &self,
        request: &ColdRequest,
        password: String,
    ) -> Result<ColdMessage, ColdFailure> {
        let password = Zeroizing::new(password);
        let (keys, network) = self.signer()?;
        match request.envelope.kind {
            Kind::SyncRequest => {
                let (answer, _) = cold::answer_sync(&keys, network, &request.envelope)?;
                Ok(ColdMessage::of(&answer))
            }
            Kind::SignRequest => {
                let id = self.inner.with(|w| Ok(w.entry.id.clone()))?;
                store()?
                    .unlock(&id, password.as_bytes())
                    .map_err(WalletError::from)?;
                let review = request
                    .review
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                    .ok_or(ColdFailure::AlreadyUsed)?;
                let (answer, _) = cold::sign(&keys, network, review)?;
                Ok(ColdMessage::of(&answer))
            }
            _ => Err(ColdFailure::WrongKind),
        }
    }

    fn watching(&self) -> Result<(kn_keys::WalletKeys, kn_keys::Network), ColdFailure> {
        let (keys, network) = self.inner.with(|w| Ok((w.keys.clone(), w.entry.network)))?;
        if !keys.is_view_only() {
            return Err(ColdFailure::NotWatching);
        }
        Ok((keys, network))
    }

    fn signer(&self) -> Result<(kn_keys::WalletKeys, kn_keys::Network), ColdFailure> {
        let (keys, network) = self.inner.with(|w| Ok((w.keys.clone(), w.entry.network)))?;
        if keys.is_view_only() {
            return Err(ColdFailure::ViewOnly);
        }
        Ok((keys, network))
    }
}

#[cfg(test)]
mod tests {
    use super::super::network::Network;
    use super::super::wallets::{SeedFormat, create_wallet_from_seed, generate_seed};
    use super::*;

    fn wallet(name: &str) -> OpenWallet {
        create_wallet_from_seed(
            name.into(),
            Network::Stagenet,
            SyncMode::Full,
            generate_seed(SeedFormat::Polyseed).words.join(" "),
            "pw".into(),
            Some(5),
            true,
        )
        .unwrap()
    }

    #[test]
    fn pairing_and_requests_between_cold_and_watching_wallets() {
        crate::test_store::init();
        let cold = wallet("Cold");
        cold.set_cold(true).unwrap();
        assert!(cold.is_cold().unwrap());
        assert!(cold.summary().unwrap().cold);

        // Pairing needs the password, and creates a matching watching wallet.
        assert_eq!(
            cold.cold_pairing("nope".into()).unwrap_err(),
            ColdFailure::WrongPassword
        );
        let pairing = cold.cold_pairing("pw".into()).unwrap();
        assert_eq!(pairing.kind, ColdKind::Pairing);
        let reader = FrameReader::new();
        let mut progress = None;
        for frame in &pairing.frames {
            progress = Some(reader.add(frame.clone()).unwrap());
        }
        assert!(progress.unwrap().complete);
        let scanned = reader.message().unwrap();
        assert_eq!(scanned, pairing.file);
        let watching =
            create_watching_wallet("Watching".into(), scanned, "pw2".into(), SyncMode::Full)
                .unwrap();
        let summary = watching.summary().unwrap();
        assert!(summary.view_only);
        assert_eq!(
            watching.addresses().unwrap()[0].address,
            cold.addresses().unwrap()[0].address
        );
        // A view-only wallet cannot become cold, and signs nothing.
        assert_eq!(watching.set_cold(true).unwrap_err(), ColdFailure::ViewOnly);
        assert_eq!(
            watching.cold_pairing("pw2".into()).unwrap_err(),
            ColdFailure::ViewOnly
        );
        assert_eq!(
            cold.cold_sync_request().unwrap_err(),
            ColdFailure::NotWatching
        );

        // A sync round trip.
        let request = watching.cold_sync_request().unwrap();
        assert_eq!(
            cold_message_kind(request.file.clone()),
            Ok(ColdKind::SyncRequest)
        );
        let read = cold.read_cold_request(request.file.clone()).unwrap();
        assert_eq!(read.summary().kind, ColdKind::SyncRequest);
        let answer = cold.answer_cold_request(&read, String::new()).unwrap();
        assert_eq!(answer.kind, ColdKind::SyncAnswer);
        let imported = watching
            .import_cold_answer(answer.file, Vec::new())
            .unwrap();
        assert_eq!(imported.key_images, 0);
        assert_eq!(imported.published_tx, None);

        // Another wallet's cold device refuses the request.
        let stranger = wallet("Stranger");
        assert_eq!(
            stranger.read_cold_request(request.file).unwrap_err(),
            ColdFailure::WrongWallet
        );
        // And a non-Kilonova code is refused outright.
        assert_eq!(
            cold_message_kind(b"monero:4abc".to_vec()),
            Err(ColdFailure::NotColdData)
        );

        cold.set_cold(false).unwrap();
        assert!(!cold.is_cold().unwrap());
    }
}
