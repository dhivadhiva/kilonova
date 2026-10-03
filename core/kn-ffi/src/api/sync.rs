//! Syncing an unlocked wallet in the background, and what it found.
//!
//! Full-mode wallets scan blocks from a node (`kn_sync::sync`); LWS-mode
//! wallets ask a light wallet server and check its answer
//! (`kn_sync::lws_sync`). Both fill the same state, so balance and history
//! do not care which mode produced them.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use flutter_rust_bridge::frb;
use kn_keys::WalletKeys;
use kn_store::{SyncMode as StoreSyncMode, UnlockedWallet};
use kn_sync::{
    Direction, LwsServer, MoneroDaemonHttp, NodeUrl, SyncCache, SyncError, SyncState,
    approximate_height, connect, lws_sync, sync_with,
};
use tokio::sync::Notify;

use super::network::Network;
use super::nodes::{RUNTIME, current_node, lws_server};
use super::wallets::{Inner, OpenWallet, WalletError, store};
use crate::frb_generated::StreamSink;

/// How long to wait at the chain tip before looking for new blocks.
const FOLLOW_INTERVAL: Duration = Duration::from_secs(30);
/// How often to ask a light wallet server while it is still catching up.
const LWS_CATCH_UP_INTERVAL: Duration = Duration::from_secs(3);
/// The shortest wait between looks while the app is in the background: a
/// block comes every two minutes, and nobody is watching.
const BACKGROUND_INTERVAL: Duration = Duration::from_mins(5);
/// The longest wait between retries of a node that cannot be reached.
const MAX_RETRY_INTERVAL: Duration = Duration::from_mins(5);
/// How often the cache is written while scanning. A crash loses at most
/// this much scanning; writing after every batch wore storage and battery.
const SAVE_INTERVAL: Duration = Duration::from_secs(10);

/// Whether the app is in the foreground; see [`set_sync_pace`].
static FOREGROUND: AtomicBool = AtomicBool::new(true);
/// Wakes waiting syncs when the pace changes.
static PACE: Notify = Notify::const_new();

/// Tells sync whether the app is in the foreground. In the background,
/// wallets at the chain tip look for new blocks every five minutes instead
/// of every thirty seconds (a sync still catching up keeps going); coming
/// back to the foreground looks right away if a look is due.
#[frb(sync)]
pub fn set_sync_pace(foreground: bool) {
    let was = FOREGROUND.swap(foreground, Ordering::Relaxed);
    if foreground && !was {
        PACE.notify_waiters();
    }
}

/// How long to wait before the next look, given what the round asked for.
fn paced(wait: Duration, foreground: bool) -> Duration {
    if foreground {
        wait
    } else {
        wait.max(BACKGROUND_INTERVAL)
    }
}

/// Wait before retrying a node after `failures` failed rounds in a row:
/// doubling from [`FOLLOW_INTERVAL`] up to [`MAX_RETRY_INTERVAL`].
fn retry_interval(failures: u32) -> Duration {
    FOLLOW_INTERVAL
        .saturating_mul(1 << failures.saturating_sub(1).min(8))
        .min(MAX_RETRY_INTERVAL)
}

/// Why sync stopped with an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncFailure {
    /// The node or server could not be reached or misbehaved; it retries on
    /// its own.
    NodeUnreachable,
    /// The selected node or server serves another network.
    WrongNetwork,
    /// The selected node address is invalid.
    BadNode,
    /// An LWS-mode wallet, but no light wallet server is set for its
    /// network.
    LwsServerNotSet,
    /// The owner has not yet agreed to share the view key with the server.
    LwsConsentNeeded,
    /// The server refused this wallet.
    LwsDenied,
    /// The server does not accept new wallets.
    LwsCreationRefused,
    /// The node or server is an onion address and no Tor proxy is set.
    NeedsTor,
    /// The light wallet server is plain http on another machine; the view
    /// key is not sent.
    InsecureLws,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SyncPhase {
    Connecting,
    Scanning,
    Synced,
    Failed,
    Stopped,
}

/// A progress report from a running sync. Flat rather than an enum with
/// data, which keeps the Dart side free of code generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncEvent {
    pub phase: SyncPhase,
    /// Blocks scanned so far.
    pub scanned: u64,
    /// Blocks the node (or the server's node) has.
    pub tip: u64,
    /// The node or server being used, while connecting.
    pub node: Option<String>,
    pub failure: Option<SyncFailure>,
    /// LWS only: outputs the server reported that are not this wallet's
    /// and were ignored.
    pub rejected_outputs: u32,
    /// LWS only: the server is still importing history from before the
    /// wallet was registered.
    pub import_pending: bool,
    /// The node's chain differs from an independent node's: it may be
    /// feeding this wallet a chain of its own.
    pub node_disagrees: bool,
}

impl SyncEvent {
    fn new(phase: SyncPhase) -> Self {
        Self {
            phase,
            scanned: 0,
            tip: 0,
            node: None,
            failure: None,
            rejected_outputs: 0,
            import_pending: false,
            node_disagrees: false,
        }
    }

    fn failed(failure: SyncFailure) -> Self {
        Self {
            failure: Some(failure),
            ..Self::new(SyncPhase::Failed)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WalletBalance {
    /// Atomic units (1 XMR = 10^12).
    pub total: u64,
    pub unlocked: u64,
    /// Incoming payments waiting in the transaction pool; not in `total`.
    pub incoming: u64,
}

// Flat flags rather than enums with data keep the Dart side free of code
// generation.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryItem {
    pub tx_hash: String,
    pub height: u64,
    pub incoming: bool,
    /// Atomic units.
    pub amount: u64,
    pub miner: bool,
    /// True while the received funds cannot be spent yet.
    pub locked: bool,
    /// Index of the first receiving subaddress in its account, if incoming.
    pub subaddress_index: Option<u32>,
    /// Not yet in a block: sent from this wallet, or incoming and waiting
    /// in the transaction pool.
    pub pending: bool,
    /// The owner's note.
    pub note: Option<String>,
    /// For transactions this wallet sent: the first recipient, by contact
    /// name if it is in the address book, else by address.
    pub sent_to: Option<String>,
}

/// Sync state shared between the wallet and its background task.
pub(crate) struct SyncHandle {
    state: Mutex<SyncState>,
    pub(crate) tip: AtomicU64,
    /// Cancel flag of the current run. Starting a run cancels the previous
    /// one.
    current: Mutex<Arc<AtomicBool>>,
    /// Incremented per run; a run only writes state while it is current, so
    /// a cancelled run that is still finishing cannot overwrite a newer one.
    generation: AtomicU64,
    /// Set once a run reaches the chain tip; sending waits for it so it
    /// never builds on a stale view of the wallet.
    pub(crate) caught_up: AtomicBool,
    /// `state` has changes the cache file does not.
    dirty: AtomicBool,
    last_save: Mutex<Option<Instant>>,
    /// Wakes a waiting run when it is stopped or replaced.
    wake: Notify,
}

impl SyncHandle {
    pub(crate) fn load(wallet: &UnlockedWallet) -> Self {
        let cached = store()
            .ok()
            .and_then(|s| s.load_cache(wallet).ok().flatten())
            .and_then(|bytes| SyncState::from_bytes(&bytes).ok());
        let state = cached.unwrap_or_else(|| starting_state(wallet));
        Self {
            tip: AtomicU64::new(state.next_height),
            state: Mutex::new(state),
            current: Mutex::new(Arc::new(AtomicBool::new(true))),
            generation: AtomicU64::new(0),
            caught_up: AtomicBool::new(false),
            dirty: AtomicBool::new(false),
            last_save: Mutex::new(None),
            wake: Notify::new(),
        }
    }

    /// Forgets everything scanned, for example after switching mode.
    pub(crate) fn reset(&self, inner: &Inner) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.caught_up.store(false, Ordering::Relaxed);
        if let Ok(state) = inner.with(|w| Ok(starting_state(w))) {
            self.tip.store(state.next_height, Ordering::Relaxed);
            *self.lock_state() = state;
            self.dirty.store(false, Ordering::Relaxed);
        }
        self.wake.notify_waiters();
    }

    pub(crate) fn stop(&self) {
        self.lock_current().store(true, Ordering::Relaxed);
        self.wake.notify_waiters();
    }

    /// Stops the current run and keeps it from writing state again, so the
    /// caller can change the state; start sync afterwards.
    pub(crate) fn pause(&self) {
        self.generation.fetch_add(1, Ordering::AcqRel);
        self.stop();
    }

    /// The scanned state and the chain tip, if sync has reached the tip.
    pub(crate) fn caught_up_state(&self) -> Option<(SyncState, u64)> {
        let tip = self.tip.load(Ordering::Relaxed);
        let caught_up =
            self.caught_up.load(Ordering::Relaxed) && self.read(|state| state.next_height >= tip);
        caught_up.then(|| (self.snapshot(), tip))
    }

    /// Replaces the state and saves the encrypted cache.
    pub(crate) fn replace(&self, inner: &Inner, state: SyncState) {
        let bytes = state.to_bytes();
        *self.lock_state() = state;
        self.dirty.store(false, Ordering::Relaxed);
        self.save_bytes(inner, &bytes);
    }

    /// Takes in a run's state; the cache is written when `force`d or when
    /// [`SAVE_INTERVAL`] has passed since the last write, and only if
    /// something changed.
    fn update(&self, inner: &Inner, state: &SyncState, force: bool) {
        {
            let mut shared = self.lock_state();
            if *shared != *state {
                shared.clone_from(state);
                self.dirty.store(true, Ordering::Relaxed);
            }
        }
        let due = force
            || self
                .lock_last_save()
                .is_none_or(|at| at.elapsed() >= SAVE_INTERVAL);
        if due {
            self.flush(inner);
        }
    }

    /// Writes the cache if the state changed since it was last written.
    pub(crate) fn flush(&self, inner: &Inner) {
        if !self.dirty.swap(false, Ordering::Relaxed) {
            return;
        }
        let bytes = self.lock_state().to_bytes();
        self.save_bytes(inner, &bytes);
    }

    fn save_bytes(&self, inner: &Inner, bytes: &[u8]) {
        // A lost cache only costs a rescan, so a failed save is not worth
        // stopping for.
        let _ = inner.with(|w| store()?.save_cache(w, bytes).map_err(WalletError::from));
        *self.lock_last_save() = Some(Instant::now());
    }

    fn lock_last_save(&self) -> std::sync::MutexGuard<'_, Option<Instant>> {
        self.last_save
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Reads the state without copying it.
    pub(crate) fn read<T>(&self, f: impl FnOnce(&SyncState) -> T) -> T {
        f(&self.lock_state())
    }

    fn lock_state(&self) -> std::sync::MutexGuard<'_, SyncState> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn lock_current(&self) -> std::sync::MutexGuard<'_, Arc<AtomicBool>> {
        self.current
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub(crate) fn snapshot(&self) -> SyncState {
        self.lock_state().clone()
    }
}

/// Where scanning starts without a cache: the restore height, else the
/// Polyseed birthday, else genesis.
fn starting_state(wallet: &UnlockedWallet) -> SyncState {
    let start = wallet.data.restore_height.unwrap_or_else(|| {
        wallet
            .data
            .birthday
            .map_or(0, |b| approximate_height(wallet.entry.network, b))
    });
    SyncState::starting_at(start)
}

/// A block height to record as the restore height of a wallet created now,
/// so a new wallet does not scan old blocks.
#[frb(sync)]
#[must_use]
pub fn restore_height_for_new_wallet(network: Network) -> u64 {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_secs());
    approximate_height(network.into(), now)
}

impl OpenWallet {
    /// Starts syncing in the background and reports through `sink`. Keeps
    /// following new blocks until [`OpenWallet::stop_sync`] or
    /// [`OpenWallet::lock`]. Calling it again restarts sync.
    pub fn start_sync(&self, sink: StreamSink<SyncEvent>) {
        self.start_sync_with(move |event| {
            let _ = sink.add(event);
        });
    }

    /// [`OpenWallet::start_sync`] with any receiver of the events.
    #[frb(ignore)]
    pub(crate) fn start_sync_with(&self, sink: impl Fn(SyncEvent) + Send + Sync + 'static) {
        // A cold wallet never goes online.
        if self.inner.with(|w| Ok(w.entry.cold)).unwrap_or(true) {
            return;
        }
        let inner = self.inner.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        {
            let mut current = inner.sync.lock_current();
            current.store(true, Ordering::Relaxed);
            *current = cancel.clone();
        }
        let generation = inner.sync.generation.fetch_add(1, Ordering::AcqRel) + 1;
        inner.sync.wake.notify_waiters();
        RUNTIME.spawn(async move {
            let run = Run {
                inner: &inner,
                sink: &sink,
                cancel: &cancel,
                generation,
                failed: AtomicBool::new(false),
                opinion_done: AtomicBool::new(false),
                disagrees: AtomicBool::new(false),
                failures: AtomicU32::new(0),
            };
            run.go().await;
            // After a failure the failure stays on screen, since it says
            // what to do next; "stopped" is only for a deliberate stop.
            if run.is_current() && !run.failed.load(Ordering::Relaxed) {
                sink(SyncEvent::new(SyncPhase::Stopped));
            }
        });
    }

    /// Stops background sync after the current step.
    #[frb(sync)]
    pub fn stop_sync(&self) {
        self.inner.sync.stop();
    }

    /// Balance from what has been scanned so far.
    #[frb(sync)]
    #[must_use]
    pub fn balance(&self) -> WalletBalance {
        let tip = self.inner.sync.tip.load(Ordering::Relaxed);
        let b = self.inner.sync.read(|state| state.balance(tip));
        WalletBalance {
            total: b.total,
            unlocked: b.unlocked,
            incoming: b.incoming,
        }
    }

    /// Transactions found so far, newest first.
    #[frb(sync)]
    #[must_use]
    pub fn history(&self) -> Vec<HistoryItem> {
        let tip = self.inner.sync.tip.load(Ordering::Relaxed);
        let history = self.inner.sync.read(SyncState::history);
        // Notes and recipients live in the wallet file; copy what history
        // needs so the wallet lock is held briefly.
        let (notes, sent_to) = self
            .inner
            .with(|w| {
                let name_of = |address: &str| {
                    w.data
                        .contacts
                        .iter()
                        .find(|c| c.address == address)
                        .map_or_else(|| address.to_owned(), |c| c.name.clone())
                };
                let sent_to: std::collections::HashMap<String, String> = w
                    .data
                    .sent
                    .iter()
                    .filter_map(|(tx, s)| Some((tx.clone(), name_of(&s.destinations.first()?.0))))
                    .collect();
                Ok((w.data.notes.clone(), sent_to))
            })
            .unwrap_or_default();
        history
            .into_iter()
            .map(|h| {
                let lock = if h.miner {
                    kn_sync::MINER_LOCK_BLOCKS
                } else {
                    kn_sync::DEFAULT_LOCK_BLOCKS
                };
                let tx_hash = hex_string(&h.tx);
                HistoryItem {
                    height: h.height,
                    incoming: h.direction == Direction::Incoming,
                    amount: h.amount,
                    miner: h.miner,
                    locked: h.direction == Direction::Incoming
                        && !h.pending
                        && tip < h.height.saturating_add(lock),
                    subaddress_index: h.subaddresses.first().map(|(_, index)| *index),
                    pending: h.pending,
                    note: notes.get(&tx_hash).cloned(),
                    sent_to: sent_to.get(&tx_hash).cloned(),
                    tx_hash,
                }
            })
            .collect()
    }
}

/// One background sync run.
struct Run<'a> {
    inner: &'a Inner,
    sink: &'a (dyn Fn(SyncEvent) + Send + Sync),
    cancel: &'a AtomicBool,
    generation: u64,
    /// Set when the run ends because of a failure it reported.
    failed: AtomicBool,
    /// Whether this run compared its node with an independent one yet.
    opinion_done: AtomicBool,
    /// The node disagreed with the independent one.
    disagrees: AtomicBool,
    /// Rounds in a row that could not reach the node or server.
    failures: AtomicU32,
}

/// Connections a run keeps between rounds, so following the tip does not
/// connect (and, over https or Tor, handshake) again every time.
#[frb(ignore)]
#[derive(Default)]
struct Connections {
    node: Option<NodeConnection>,
    lws: Option<(NodeUrl, LwsServer)>,
}

#[frb(ignore)]
struct NodeConnection {
    url: NodeUrl,
    daemon: MoneroDaemonHttp,
    cache: SyncCache,
}

/// What a run needs from the wallet, copied out so the wallet lock is not
/// held while talking to the network.
struct Setup {
    network: kn_keys::Network,
    mode: StoreSyncMode,
    keys: WalletKeys,
    accounts: Vec<u32>,
    restore_height: u64,
    created_here: bool,
    lws_consent: Option<String>,
}

impl Run<'_> {
    fn is_current(&self) -> bool {
        self.inner.sync.generation.load(Ordering::Acquire) == self.generation
    }

    fn stopped(&self) -> bool {
        self.cancel.load(Ordering::Relaxed) || !self.is_current()
    }

    fn emit(&self, mut event: SyncEvent) {
        event.node_disagrees = self.disagrees.load(Ordering::Relaxed);
        self.failed
            .store(event.phase == SyncPhase::Failed, Ordering::Relaxed);
        if self.is_current() {
            if event.phase == SyncPhase::Synced {
                self.inner.sync.caught_up.store(true, Ordering::Relaxed);
            }
            (self.sink)(event);
        }
    }

    /// Stores progress if this run is current; the encrypted cache is
    /// written now if `force`d, else at most every [`SAVE_INTERVAL`].
    fn record(&self, state: &SyncState, tip: u64, force: bool) {
        if !self.is_current() {
            return;
        }
        self.inner.sync.tip.store(tip, Ordering::Relaxed);
        self.inner.sync.update(self.inner, state, force);
    }

    async fn go(&self) {
        let Ok(setup) = self.inner.with(|w| {
            Ok(Setup {
                network: w.entry.network,
                mode: w.entry.mode,
                keys: w.keys.clone(),
                accounts: w.data.next_subaddress.clone(),
                restore_height: starting_state(w).next_height,
                created_here: w.data.created_here,
                lws_consent: w.data.lws_consent.clone(),
            })
        }) else {
            return;
        };
        let mut connections = Connections::default();
        while !self.stopped() {
            let wait = match setup.mode {
                StoreSyncMode::Full => self.full_round(&setup, &mut connections).await,
                StoreSyncMode::Lws => self.lws_round(&setup, &mut connections).await,
            };
            let Some(wait) = wait else { break };
            if !self.wait(wait).await {
                break;
            }
        }
        // Scanning since the last write is kept, unless another run or a
        // send has taken over the state.
        if self.is_current() {
            self.inner.sync.flush(self.inner);
        }
    }

    /// Sleeps for `wait`, longer in the background (see [`set_sync_pace`]).
    /// Returns `false` as soon as the run is stopped, without polling for
    /// it.
    async fn wait(&self, wait: Duration) -> bool {
        let start = Instant::now();
        loop {
            let woken = self.inner.sync.wake.notified();
            let paced_again = PACE.notified();
            tokio::pin!(woken, paced_again);
            // Registered before the checks, so a wake between them is kept.
            woken.as_mut().enable();
            paced_again.as_mut().enable();
            if self.stopped() {
                return false;
            }
            let due = start + paced(wait, FOREGROUND.load(Ordering::Relaxed));
            let Some(left) = due.checked_duration_since(Instant::now()) else {
                return true;
            };
            tokio::select! {
                () = tokio::time::sleep(left) => {}
                () = woken => {}
                () = paced_again => {}
            }
        }
    }

    /// One full-mode pass to the tip. Returns how long to wait before the
    /// next, or `None` to stop.
    async fn full_round(&self, setup: &Setup, connections: &mut Connections) -> Option<Duration> {
        let Ok(node) = current_node(setup.network.into()) else {
            self.emit(SyncEvent::failed(SyncFailure::BadNode));
            return None;
        };
        self.emit(SyncEvent {
            node: Some(node.as_str().to_owned()),
            ..SyncEvent::new(SyncPhase::Connecting)
        });
        let mut state = self.inner.sync.snapshot();
        let result = async {
            let (daemon, cache) = self
                .node(&node, setup.network, &mut connections.node)
                .await?;
            sync_with(
                daemon,
                cache,
                &setup.keys,
                &setup.accounts,
                &mut state,
                self.cancel,
                |state, progress| {
                    self.record(state, progress.tip, false);
                    self.emit(SyncEvent {
                        scanned: progress.scanned,
                        tip: progress.tip,
                        ..SyncEvent::new(SyncPhase::Scanning)
                    });
                },
            )
            .await
        }
        .await;
        match result {
            Ok(()) => {
                self.failures.store(0, Ordering::Relaxed);
                let tip = self.inner.sync.tip.load(Ordering::Relaxed);
                // At the tip, sync also expires dropped spends and reads the
                // transaction pool; keep that too.
                self.record(&state, tip, true);
                self.emit(SyncEvent {
                    scanned: tip,
                    tip,
                    ..SyncEvent::new(SyncPhase::Synced)
                });
                Some(FOLLOW_INTERVAL)
            }
            Err(e) => {
                // A node that failed is connected afresh next time.
                connections.node = None;
                self.fail(&e)
            }
        }
    }

    /// The connection to `node` kept from an earlier round, or a new one.
    /// A new connection is compared with an independent node once per run.
    async fn node<'c>(
        &self,
        node: &NodeUrl,
        network: kn_keys::Network,
        kept: &'c mut Option<NodeConnection>,
    ) -> Result<(&'c MoneroDaemonHttp, &'c mut SyncCache), SyncError> {
        if kept.as_ref().is_none_or(|k| k.url != *node) {
            *kept = None;
            let (daemon, status) = connect(node, network).await?;
            if !status.test_chain && !self.opinion_done.swap(true, Ordering::Relaxed) {
                self.second_opinion(&daemon, node, network).await;
            }
            *kept = Some(NodeConnection {
                url: node.clone(),
                daemon,
                cache: SyncCache::default(),
            });
        }
        let kept = kept.as_mut().expect("connected above");
        Ok((&kept.daemon, &mut kept.cache))
    }

    /// Compares the node with a bundled one it is not, once per run. An
    /// answer that cannot be had is not a disagreement.
    async fn second_opinion(
        &self,
        daemon: &kn_sync::MoneroDaemonHttp,
        node: &NodeUrl,
        network: kn_keys::Network,
    ) {
        let Some(reference) = kn_sync::bundled_nodes(network)
            .into_iter()
            .find(|n| n != node)
        else {
            return;
        };
        let opinion = tokio::time::timeout(
            Duration::from_secs(30),
            kn_sync::second_opinion(daemon, &reference, network),
        )
        .await;
        if let Ok(Ok(opinion)) = opinion
            && !opinion.agrees
        {
            self.disagrees.store(true, Ordering::Relaxed);
        }
    }

    /// When the owner asked for it, confirms the server's payments with the
    /// network's node. Returns how many outputs the node contradicted; a
    /// node that cannot be reached confirms nothing and contradicts nothing.
    async fn confirm_with_node(
        &self,
        network: kn_keys::Network,
        state: &mut SyncState,
        kept: &mut Option<NodeConnection>,
    ) -> u32 {
        let enabled = crate::node_settings::load().is_ok_and(|s| s.confirm_lws_payments);
        if !enabled {
            return 0;
        }
        let Ok(node) = current_node(network.into()) else {
            return 0;
        };
        let before = state.outputs.len();
        let checked = async {
            if kept.as_ref().is_none_or(|k| k.url != node) {
                let (daemon, _) = connect(&node, network).await?;
                *kept = Some(NodeConnection {
                    url: node.clone(),
                    daemon,
                    cache: SyncCache::default(),
                });
            }
            let daemon = &kept.as_ref().expect("connected above").daemon;
            kn_sync::cross_check(daemon, state).await
        };
        match tokio::time::timeout(Duration::from_mins(1), checked).await {
            Ok(Ok(_)) => {}
            Ok(Err(_)) => {
                *kept = None;
                return 0;
            }
            Err(_) => return 0,
        }
        u32::try_from(before.saturating_sub(state.outputs.len())).unwrap_or(u32::MAX)
    }

    /// One request round to the light wallet server.
    async fn lws_round(&self, setup: &Setup, connections: &mut Connections) -> Option<Duration> {
        let Ok(Some(server_url)) = lws_server(setup.network.into()) else {
            self.emit(SyncEvent::failed(SyncFailure::LwsServerNotSet));
            return None;
        };
        // The view key goes to the server only with the owner's agreement
        // for this exact server.
        if setup.lws_consent.as_deref() != Some(server_url.as_str()) {
            self.emit(SyncEvent {
                node: Some(server_url),
                ..SyncEvent::failed(SyncFailure::LwsConsentNeeded)
            });
            return None;
        }
        let Ok(url) = NodeUrl::parse(&server_url) else {
            self.emit(SyncEvent::failed(SyncFailure::BadNode));
            return None;
        };
        self.emit(SyncEvent {
            node: Some(server_url.clone()),
            ..SyncEvent::new(SyncPhase::Connecting)
        });
        let mut state = self.inner.sync.snapshot();
        let result = async {
            if connections
                .lws
                .as_ref()
                .is_none_or(|(kept, _)| *kept != url)
            {
                connections.lws = Some((url.clone(), LwsServer::new(&url)?));
            }
            let (_, server) = connections.lws.as_ref().expect("connected above");
            lws_sync(
                server,
                &setup.keys,
                setup.network,
                &setup.accounts,
                setup.restore_height,
                setup.created_here,
                &mut state,
            )
            .await
        }
        .await;
        let result = match result {
            Ok(mut report) => {
                report.rejected_outputs = report.rejected_outputs.saturating_add(
                    self.confirm_with_node(setup.network, &mut state, &mut connections.node)
                        .await,
                );
                Ok(report)
            }
            Err(e) => Err(e),
        };
        match result {
            Ok(report) => {
                self.failures.store(0, Ordering::Relaxed);
                self.record(&state, report.tip, true);
                let caught_up = report.scanned >= report.tip;
                self.emit(SyncEvent {
                    scanned: report.scanned,
                    tip: report.tip,
                    rejected_outputs: report.rejected_outputs,
                    import_pending: report.import_pending,
                    ..SyncEvent::new(if caught_up {
                        SyncPhase::Synced
                    } else {
                        SyncPhase::Scanning
                    })
                });
                Some(if caught_up {
                    FOLLOW_INTERVAL
                } else {
                    LWS_CATCH_UP_INTERVAL
                })
            }
            Err(e) => {
                connections.lws = None;
                self.fail(&e)
            }
        }
    }

    /// Reports `e`; network trouble is retried, less often the longer it
    /// lasts, and configuration problems stop.
    fn fail(&self, e: &SyncError) -> Option<Duration> {
        let failure = match e {
            SyncError::Cancelled => return None,
            SyncError::Node(_) => SyncFailure::NodeUnreachable,
            SyncError::WrongNetwork => SyncFailure::WrongNetwork,
            SyncError::BadNodeUrl | SyncError::BadProxyUrl => SyncFailure::BadNode,
            SyncError::LwsDenied => SyncFailure::LwsDenied,
            SyncError::LwsCreationRefused => SyncFailure::LwsCreationRefused,
            SyncError::NeedsProxy => SyncFailure::NeedsTor,
            SyncError::InsecureLws => SyncFailure::InsecureLws,
        };
        self.emit(SyncEvent::failed(failure));
        (failure == SyncFailure::NodeUnreachable)
            .then(|| retry_interval(self.failures.fetch_add(1, Ordering::Relaxed) + 1))
    }
}

pub(crate) fn hex_string(bytes: &[u8; 32]) -> String {
    use std::fmt::Write as _;
    bytes.iter().fold(String::with_capacity(64), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_background_pace_only_lengthens_waits() {
        assert_eq!(paced(FOLLOW_INTERVAL, true), FOLLOW_INTERVAL);
        assert_eq!(paced(LWS_CATCH_UP_INTERVAL, true), LWS_CATCH_UP_INTERVAL);
        assert_eq!(paced(FOLLOW_INTERVAL, false), BACKGROUND_INTERVAL);
        assert_eq!(
            paced(Duration::from_mins(20), false),
            Duration::from_mins(20)
        );
    }

    #[test]
    fn retries_back_off_up_to_a_limit() {
        let waits: Vec<u64> = (1..=6).map(|n| retry_interval(n).as_secs()).collect();
        assert_eq!(waits, [30, 60, 120, 240, 300, 300]);
        assert_eq!(retry_interval(0), FOLLOW_INTERVAL);
        assert_eq!(retry_interval(u32::MAX), MAX_RETRY_INTERVAL);
    }

    #[test]
    fn coming_to_the_foreground_wakes_waiting_syncs() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            set_sync_pace(false);
            let woken = PACE.notified();
            tokio::pin!(woken);
            woken.as_mut().enable();
            set_sync_pace(true);
            tokio::time::timeout(Duration::from_secs(1), woken)
                .await
                .expect("woken");
        });
    }

    #[test]
    fn the_cache_is_written_when_due_and_only_when_changed() {
        use super::super::wallets::{SyncMode, create_wallet_from_seed, generate_seed};
        crate::test_store::init();
        let seed = generate_seed(super::super::wallets::SeedFormat::Polyseed);
        let wallet = create_wallet_from_seed(
            "Saves".into(),
            Network::Stagenet,
            SyncMode::Full,
            seed.words.join(" "),
            "pw".into(),
            Some(100),
            true,
        )
        .unwrap();
        let inner = &wallet.inner;
        let cached = || {
            inner
                .with(|w| Ok(store()?.load_cache(w)?))
                .unwrap()
                .map(|bytes| SyncState::from_bytes(&bytes).unwrap().next_height)
        };
        let id = wallet.summary().unwrap().id;

        // The first change is written at once.
        inner
            .sync
            .update(inner, &SyncState::starting_at(101), false);
        assert_eq!(cached(), Some(101));
        // Later ones wait for the interval, or a flush.
        inner
            .sync
            .update(inner, &SyncState::starting_at(102), false);
        assert_eq!(cached(), Some(101));
        assert_eq!(inner.sync.read(|s| s.next_height), 102);
        inner.sync.flush(inner);
        assert_eq!(cached(), Some(102));
        // Nothing changed: nothing is written, even when forced.
        store().unwrap().remove_cache(&id).unwrap();
        inner.sync.update(inner, &SyncState::starting_at(102), true);
        assert_eq!(cached(), None);
        // A forced change is written at once; locking writes what is left.
        inner.sync.update(inner, &SyncState::starting_at(103), true);
        assert_eq!(cached(), Some(103));
        inner
            .sync
            .update(inner, &SyncState::starting_at(104), false);
        assert_eq!(cached(), Some(103));
        let reopened = inner.with(|w| Ok(w.entry.id.clone())).unwrap();
        wallet.lock();
        let again = super::super::wallets::unlock_wallet(reopened, "pw".into()).unwrap();
        assert_eq!(again.inner.sync.read(|s| s.next_height), 104);
    }
}
