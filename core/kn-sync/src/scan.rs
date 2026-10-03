//! Scanning blocks from a node into a wallet's [`SyncState`].

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};

use kn_keys::WalletKeys;
use monero_daemon_rpc::MoneroDaemon;
use monero_interface::{
    ProvidesBlockchain as _, ProvidesBlockchainMeta as _, ProvidesTransactions as _, ScannableBlock,
};
use monero_wallet::{Scanner, address::SubaddressIndex, block::Block, transaction::Input};

use crate::{
    SyncError,
    node::Http,
    state::{OwnedOutput, PoolPayment, Spend, SyncState},
};

/// Blocks fetched per request. Small enough that progress moves and a
/// cancel is noticed quickly, large enough to keep request overhead low.
const BATCH: u64 = 50;

/// Subaddresses watched beyond the highest one handed out, so payments to
/// addresses created on another device are still found.
pub const SUBADDRESS_LOOKAHEAD: u32 = 50;

/// Where a sync is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    /// Next block to scan.
    pub scanned: u64,
    /// Number of blocks the node has.
    pub tip: u64,
}

/// What a sync keeps between calls with the same node, so following the
/// tip stays cheap: pool transactions already looked at, and a block to
/// scan the pool in.
#[derive(Default)]
pub struct SyncCache {
    /// Pool transactions by hash, as last seen.
    pool: HashMap<[u8; 32], PoolTx>,
    /// Any block; pool transactions ride in it to be scanned.
    carrier: Option<Block>,
}

/// What a pool transaction means for the wallet, kept so it is fetched and
/// scanned only once while it waits.
struct PoolTx {
    /// Every key image it spends; matched against the wallet's own on each
    /// look, since a cold wallet may supply more of them later.
    key_images: Vec<[u8; 32]>,
    /// Payments to this wallet: subaddress and amount.
    received: Vec<((u32, u32), u64)>,
}

/// Scans `daemon`'s chain from `state.next_height` to its tip.
///
/// `accounts[i]` is how many subaddresses account `i` has handed out; each
/// is watched with [`SUBADDRESS_LOOKAHEAD`] more. `on_batch` is called with
/// the state after every batch, which is the moment to save it. Setting
/// `cancel` stops after the current batch; the state is consistent at every
/// batch boundary.
///
/// # Errors
///
/// [`SyncError::Cancelled`] if cancelled, otherwise node errors. `state`
/// keeps everything scanned before the error.
pub async fn sync(
    daemon: &MoneroDaemon<Http>,
    keys: &WalletKeys,
    accounts: &[u32],
    state: &mut SyncState,
    cancel: &AtomicBool,
    on_batch: impl FnMut(&SyncState, Progress),
) -> Result<(), SyncError> {
    sync_with(
        daemon,
        &mut SyncCache::default(),
        keys,
        accounts,
        state,
        cancel,
        on_batch,
    )
    .await
}

/// [`sync`], keeping `cache` for the next call with the same node.
///
/// # Errors
///
/// As [`sync`].
pub async fn sync_with(
    daemon: &MoneroDaemon<Http>,
    cache: &mut SyncCache,
    keys: &WalletKeys,
    accounts: &[u32],
    state: &mut SyncState,
    cancel: &AtomicBool,
    mut on_batch: impl FnMut(&SyncState, Progress),
) -> Result<(), SyncError> {
    let mut scanner = Scanner::new(keys.view_pair());
    for (account, handed_out) in accounts.iter().enumerate() {
        let account =
            u32::try_from(account).map_err(|_| SyncError::Node("too many accounts".into()))?;
        for index in 0..handed_out.saturating_add(SUBADDRESS_LOOKAHEAD) {
            if let Some(sub) = SubaddressIndex::new(account, index) {
                scanner.register_subaddress(sub);
            }
        }
    }

    // One request says both how long the chain is and whether the newest
    // block this wallet scanned is still on it, which is all a wallet at
    // the tip needs to know.
    let (top, top_hash) = last_block(daemon).await?;
    let mut tip = top + 1;
    if state.recent.back() != Some(&(top, top_hash)) {
        rewind_if_reorganized(daemon, state).await?;
    }
    let mut owned = key_image_index(state);
    let mut by_output_key = output_key_index(state);
    // Whether `tip` was asked for since the last batch was scanned.
    let mut tip_is_fresh = true;

    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(SyncError::Cancelled);
        }
        on_batch(
            state,
            Progress {
                scanned: state.next_height,
                tip,
            },
        );
        if state.next_height >= tip {
            if !tip_is_fresh {
                // Blocks may have arrived while scanning.
                tip = to_u64(daemon.latest_block_number().await?)? + 1;
                tip_is_fresh = true;
                if state.next_height < tip {
                    continue;
                }
            }
            state.expire_pending(tip);
            // The pool is a convenience: a node that will not show it does
            // not stop sync.
            let _ = scan_pool(daemon, cache, &mut scanner, state, &owned, tip).await;
            return Ok(());
        }
        let end = (state.next_height + BATCH).min(tip) - 1;
        let blocks =
            crate::blocks::scannable_blocks(daemon, to_usize(state.next_height)?..=to_usize(end)?)
                .await?;
        // A first block that does not build on the last one scanned means
        // the node's chain changed since: find where, and go on from there.
        if let (Some(first), Some(&(height, hash))) = (blocks.first(), state.recent.back())
            && to_u64(first.block.number())? == height + 1
            && first.block.header.previous != hash
        {
            if !rewind_if_reorganized(daemon, state).await? {
                return Err(SyncError::Node(
                    "the node's blocks do not agree with its block hashes".into(),
                ));
            }
            owned = key_image_index(state);
            by_output_key = output_key_index(state);
            tip = to_u64(daemon.latest_block_number().await?)? + 1;
            continue;
        }
        tip_is_fresh = false;
        if let Some(last) = blocks.last() {
            cache.carrier = Some(last.block.clone());
        }
        for block in blocks {
            let height = state.next_height;
            scan_block(
                &mut scanner,
                keys,
                state,
                &mut owned,
                &mut by_output_key,
                height,
                block,
            )?;
        }
    }
}

/// Height and hash of the node's newest block.
async fn last_block(daemon: &MoneroDaemon<Http>) -> Result<(u64, [u8; 32]), SyncError> {
    #[derive(serde::Deserialize)]
    struct Reply {
        block_header: Header,
    }
    #[derive(serde::Deserialize)]
    struct Header {
        height: u64,
        hash: String,
    }
    let reply = daemon
        .json_rpc_call("get_last_block_header", None, 4096)
        .await?;
    let header = serde_json::from_str::<Reply>(&reply)
        .map_err(|_| SyncError::Node("unexpected get_last_block_header response".into()))?
        .block_header;
    let hash = hex::decode(&header.hash)
        .ok()
        .and_then(|h| h.try_into().ok())
        .ok_or_else(|| SyncError::Node("unexpected block hash".into()))?;
    Ok((header.height, hash))
}

fn scan_block(
    scanner: &mut Scanner,
    keys: &WalletKeys,
    state: &mut SyncState,
    owned: &mut HashMap<[u8; 32], usize>,
    by_output_key: &mut HashMap<[u8; 32], usize>,
    height: u64,
    block: ScannableBlock,
) -> Result<(), SyncError> {
    let hash = block.block.hash();
    let miner_tx = block.block.miner_transaction().hash();

    // Spends first: an input in this block can only spend an output from an
    // earlier block.
    // Pruned transactions do not carry their hash; the block lists the
    // hashes in the same order.
    for (tx, tx_hash) in block.transactions.iter().zip(&block.block.transactions) {
        for input in &tx.prefix().inputs {
            if let Input::ToKey { key_image, .. } = input
                && let Some(&i) = owned.get(&key_image.to_bytes())
            {
                state.outputs[i].spent = Some(Spend {
                    tx: *tx_hash,
                    height,
                    pending: false,
                });
            }
        }
    }

    let found = scanner
        .scan(block)
        .map_err(|e| SyncError::Node(format!("block {height} could not be scanned: {e}")))?;
    // Additionally timelocked outputs are kept and shown; spending rules for
    // them come with sending.
    for output in found.ignore_additional_timelock() {
        let key_image = keys
            .key_image(output.key(), output.key_offset())
            .or_else(|| state.known_key_image(&output.key().compress().to_bytes()));
        let new = OwnedOutput {
            miner: output.transaction() == miner_tx,
            output,
            height,
            key_image,
            spent: None,
        };
        // Two outputs with the same one-time key share one key image, so
        // only one of them can ever be spent (the "burning bug"). Keep the
        // larger, as Monero's own wallet does, so the balance never counts
        // money that cannot be spent.
        let key = new.output.key().compress().to_bytes();
        if let Some(&i) = by_output_key.get(&key) {
            let existing = &state.outputs[i];
            if existing.spent.is_none() && new.amount() > existing.amount() {
                state.outputs[i] = new;
            }
            continue;
        }
        if let Some(ki) = key_image {
            owned.insert(ki, state.outputs.len());
        }
        by_output_key.insert(key, state.outputs.len());
        state.outputs.push(new);
    }
    state.record_block(height, hash);
    Ok(())
}

/// Most pool transactions looked at per sync. Mainnet pools rarely hold
/// more; beyond this the wallet waits for the payment to be mined.
const POOL_LIMIT: usize = 500;

/// Records payments to this wallet in the node's transaction pool, and
/// marks outputs that pool transactions spend (sent from another device
/// with the same seed) as pending spends. Only transactions not seen on an
/// earlier look are fetched.
async fn scan_pool(
    daemon: &MoneroDaemon<Http>,
    cache: &mut SyncCache,
    scanner: &mut Scanner,
    state: &mut SyncState,
    owned: &HashMap<[u8; 32], usize>,
    tip: u64,
) -> Result<(), SyncError> {
    #[derive(serde::Deserialize)]
    struct Hashes {
        #[serde(default)]
        tx_hashes: Vec<String>,
    }
    let reply = daemon
        .rpc_call("get_transaction_pool_hashes", None, 1 << 20)
        .await?;
    let hashes: Vec<[u8; 32]> = serde_json::from_str::<Hashes>(&reply)
        .map_err(|e| SyncError::Node(format!("pool: {e}")))?
        .tx_hashes
        .iter()
        .filter_map(|h| hex::decode(h).ok()?.try_into().ok())
        .take(POOL_LIMIT)
        .collect();
    // Forget what left the pool.
    cache.pool.retain(|hash, _| hashes.contains(hash));
    if hashes.is_empty() {
        state.pool.clear();
        return Ok(());
    }

    let new: Vec<[u8; 32]> = hashes
        .iter()
        .filter(|h| !cache.pool.contains_key(*h))
        .copied()
        .collect();
    if !new.is_empty() {
        learn_pool_transactions(daemon, cache, scanner, &new, tip).await?;
    }

    let mut payments: Vec<PoolPayment> = Vec::new();
    for hash in &hashes {
        let Some(tx) = cache.pool.get(hash) else {
            continue;
        };
        let spent: Vec<[u8; 32]> = tx
            .key_images
            .iter()
            .filter(|ki| owned.contains_key(*ki))
            .copied()
            .collect();
        state.mark_pending(&spent, *hash, tip);
        if tx.received.is_empty() {
            continue;
        }
        let mut payment = PoolPayment {
            tx: *hash,
            amount: 0,
            subaddresses: Vec::new(),
            by_subaddress: Vec::new(),
        };
        for &(index, amount) in &tx.received {
            payment.amount = payment.amount.saturating_add(amount);
            if !payment.subaddresses.contains(&index) {
                payment.subaddresses.push(index);
            }
            match payment
                .by_subaddress
                .iter_mut()
                .find(|(at, _)| *at == index)
            {
                Some((_, sum)) => *sum = sum.saturating_add(amount),
                None => payment.by_subaddress.push((index, amount)),
            }
        }
        payments.push(payment);
    }
    state.pool = payments;
    Ok(())
}

/// Fetches and scans pool transactions not seen before, into `cache`.
async fn learn_pool_transactions(
    daemon: &MoneroDaemon<Http>,
    cache: &mut SyncCache,
    scanner: &mut Scanner,
    new: &[[u8; 32]],
    tip: u64,
) -> Result<(), SyncError> {
    let transactions = daemon
        .pruned_transactions(new)
        .await
        .map_err(|e| SyncError::Node(format!("pool: {e:?}")))?;
    let key_images: Vec<Vec<[u8; 32]>> = transactions
        .iter()
        .map(|tx| {
            tx.prefix()
                .inputs
                .iter()
                .filter_map(|input| match input {
                    Input::ToKey { key_image, .. } => Some(key_image.to_bytes()),
                    Input::Gen(_) => None,
                })
                .collect()
        })
        .collect();

    // The scanner only reads blocks, so the pool transactions ride in a
    // block in place of its own. Output positions on the chain are
    // unknown until mined, which is fine: these outputs are only shown.
    let mut carrier = if let Some(block) = &cache.carrier {
        block.clone()
    } else {
        let block = daemon.block_by_number(to_usize(tip - 1)?).await?;
        cache.carrier = Some(block.clone());
        block
    };
    carrier.transactions = new.to_vec();
    let found = scanner.scan(ScannableBlock {
        block: carrier,
        transactions,
        output_index_for_first_ringct_output: Some(0),
    });
    let found = found.map_err(|e| SyncError::Node(format!("pool could not be scanned: {e}")))?;
    for (hash, key_images) in new.iter().zip(key_images) {
        cache.pool.insert(
            *hash,
            PoolTx {
                key_images,
                received: Vec::new(),
            },
        );
    }
    for output in found.ignore_additional_timelock() {
        // Anything else is the carrier block's miner output.
        if let Some(tx) = cache.pool.get_mut(&output.transaction())
            && new.contains(&output.transaction())
        {
            let index = output
                .subaddress()
                .map_or((0, 0), |s| (s.account(), s.address()));
            tx.received.push((index, output.commitment().amount));
        }
    }
    Ok(())
}

/// Compares the newest remembered block with the node's chain and rewinds
/// to the last common block if they differ. Returns whether it rewound.
async fn rewind_if_reorganized(
    daemon: &MoneroDaemon<Http>,
    state: &mut SyncState,
) -> Result<bool, SyncError> {
    let mut rewound = false;
    while let Some(&(height, hash)) = state.recent.back() {
        if daemon.block_hash(to_usize(height)?).await? == hash {
            break;
        }
        state.rewind_to(height);
        rewound = true;
    }
    Ok(rewound)
}

/// Owned outputs by key image, pointing into `state.outputs`.
fn output_key_index(state: &SyncState) -> HashMap<[u8; 32], usize> {
    state
        .outputs
        .iter()
        .enumerate()
        .map(|(i, o)| (o.output.key().compress().to_bytes(), i))
        .collect()
}

fn key_image_index(state: &SyncState) -> HashMap<[u8; 32], usize> {
    state
        .outputs
        .iter()
        .enumerate()
        .filter_map(|(i, o)| o.key_image.map(|ki| (ki, i)))
        .collect()
}

fn to_u64(n: usize) -> Result<u64, SyncError> {
    u64::try_from(n).map_err(|_| SyncError::Node("height out of range".into()))
}

fn to_usize(n: u64) -> Result<usize, SyncError> {
    usize::try_from(n).map_err(|_| SyncError::Node("height out of range".into()))
}
