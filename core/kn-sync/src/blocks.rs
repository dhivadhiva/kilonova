//! Fetching blocks to scan.
//!
//! monero-oxide fetches a range of blocks with one `get_blocks.bin` request,
//! but only uses it if every transaction comes with the hash of its pruned
//! data. monerod (0.18.x, mainnet nodes included) sends zeros there
//! (monero-project/monero#10120), so monero-oxide falls back to fetching
//! every block on its own, with two or three requests per block.
//!
//! This keeps the single `get_blocks.bin` request for the blocks, their
//! transactions and output indexes, and asks for the missing hashes with
//! batched `get_transactions` requests (a hundred transactions each). Every
//! transaction is still checked against the hash its block lists, exactly
//! as monero-oxide does: transactions whose hash cannot be checked from the
//! binary response are replaced by ones fetched and checked by monero-oxide.

use std::ops::RangeInclusive;

use monero_daemon_rpc::MoneroDaemon;
use monero_epee::{Epee, EpeeEntry, EpeeError};
use monero_interface::{
    ProvidesScannableBlocks as _, ProvidesTransactions as _, ScannableBlock, TransactionsError,
};
use monero_wallet::{
    block::Block,
    transaction::{Pruned, Transaction},
};

use crate::{SyncError, node::Http};

/// Upper bound on a `get_blocks.bin` response. Nodes older than 0.18.4.3
/// ignore the block count asked for and send up to 1000 blocks, so this
/// matches monero-oxide's own limit rather than the batch size.
const MAX_RESPONSE: usize = 100 * 1024 * 1024;

/// A block as `get_blocks.bin` sent it: transactions with their prunable
/// hash, if the node sent a usable one.
struct RawBlock {
    block: Block,
    transactions: Vec<(Transaction<Pruned>, Option<[u8; 32]>)>,
    first_ringct_output: Option<u64>,
}

/// Blocks `range` (block numbers), checked like monero-oxide's
/// `contiguous_scannable_blocks`: numbered as asked, each building on the
/// previous, and every transaction matching its block's list.
pub(crate) async fn scannable_blocks(
    daemon: &MoneroDaemon<Http>,
    range: RangeInclusive<usize>,
) -> Result<Vec<ScannableBlock>, SyncError> {
    let (mut start, end) = range.into_inner();
    let mut out = Vec::with_capacity(end.saturating_sub(start).saturating_add(1));
    // `get_blocks.bin` cannot start at genesis; that one comes on its own.
    if start == 0 {
        out.push(daemon.scannable_block_by_number(0).await?);
        start = 1;
    }
    let mut raw: Vec<RawBlock> = Vec::new();
    while start + raw.len() <= end {
        let from = start + raw.len();
        let wanted = end - from + 1;
        let reply = daemon
            .bin_call("get_blocks.bin", blocks_request(from, wanted), MAX_RESPONSE)
            .await?;
        let mut got = parse_blocks_bin(&reply)?;
        if got.is_empty() {
            return Err(invalid("no blocks in the answer"));
        }
        got.truncate(wanted);
        raw.extend(got);
    }

    // Transactions whose hash cannot be checked from the binary answer:
    // fetched again, and checked, by monero-oxide.
    let unchecked: Vec<[u8; 32]> = raw
        .iter()
        .flat_map(|b| b.block.transactions.iter().zip(&b.transactions))
        .filter(|(_, (_, prunable))| prunable.is_none())
        .map(|(hash, _)| *hash)
        .collect();
    let mut refetched = if unchecked.is_empty() {
        Vec::new()
    } else {
        daemon
            .pruned_transactions(&unchecked)
            .await
            .map_err(|e| match e {
                TransactionsError::InterfaceError(e) => SyncError::from(e),
                other => SyncError::Node(format!("transactions: {other:?}")),
            })?
    }
    .into_iter();

    let mut parent = out.last().map(|b: &ScannableBlock| b.block.hash());
    for (number, raw) in (start..=end).zip(raw) {
        if raw.block.number() != number {
            return Err(invalid("a block other than the one asked for"));
        }
        if parent.is_some_and(|p| p != raw.block.header.previous) {
            return Err(invalid("blocks that do not build on each other"));
        }
        parent = Some(raw.block.hash());
        if raw.transactions.len() != raw.block.transactions.len() {
            return Err(invalid("a block without all its transactions"));
        }
        let mut transactions = Vec::with_capacity(raw.transactions.len());
        for (hash, (tx, prunable)) in raw.block.transactions.iter().zip(raw.transactions) {
            let tx = match prunable {
                Some(prunable) => {
                    if tx.hash_with_prunable_hash(prunable) != Some(*hash) {
                        return Err(invalid("a transaction its block does not list"));
                    }
                    tx
                }
                None => refetched
                    .next()
                    .ok_or_else(|| invalid("too few transactions"))?,
            };
            transactions.push(tx);
        }
        out.push(ScannableBlock {
            block: raw.block,
            transactions,
            output_index_for_first_ringct_output: raw.first_ringct_output,
        });
    }
    Ok(out)
}

fn invalid(what: &str) -> SyncError {
    SyncError::Node(format!("the node sent {what}"))
}

/// `get_blocks.bin` for `count` pruned blocks from `start`.
fn blocks_request(start: usize, count: usize) -> Vec<u8> {
    fn key(out: &mut Vec<u8>, name: &str, kind: monero_epee::Type) {
        out.push(u8::try_from(name.len()).expect("short key"));
        out.extend_from_slice(name.as_bytes());
        out.push(kind as u8);
    }
    let mut out = Vec::with_capacity(80);
    out.extend_from_slice(&monero_epee::HEADER);
    out.push(monero_epee::VERSION);
    out.push(3 << 2); // three fields
    key(&mut out, "prune", monero_epee::Type::Bool);
    out.push(1);
    key(&mut out, "start_height", monero_epee::Type::Uint64);
    out.extend_from_slice(&(start as u64).to_le_bytes());
    // Nodes from 0.18.4.3 send no more than this; older ones ignore it.
    key(&mut out, "max_block_count", monero_epee::Type::Uint64);
    out.extend_from_slice(&(count as u64).to_le_bytes());
    out
}

fn epee(e: EpeeError) -> SyncError {
    SyncError::Node(format!("the node sent malformed blocks: {e:?}"))
}

/// Reads a `get_blocks.bin` answer, following monero-oxide's reader, except
/// that a missing prunable hash marks the transaction for fetching instead
/// of failing the whole answer.
fn parse_blocks_bin(bytes: &[u8]) -> Result<Vec<RawBlock>, SyncError> {
    let mut decoder = Epee::new(bytes).map_err(epee)?;
    let mut fields = decoder.entry().map_err(epee)?.fields().map_err(epee)?;
    let mut blocks = Vec::new();
    let mut output_indexes: Vec<u64> = Vec::new();
    while let Some(field) = fields.next() {
        let (key, value) = field.map_err(epee)?;
        match key.consume() {
            b"blocks" => {
                let mut entries = value.iterate().map_err(epee)?;
                while let Some(entry) = entries.next() {
                    blocks.push(parse_block(entry.map_err(epee)?)?);
                }
            }
            b"output_indices" => {
                let mut per_block = value.iterate().map_err(epee)?;
                while let Some(block) = per_block.next() {
                    let mut fields = block.map_err(epee)?.fields().map_err(epee)?;
                    while let Some(field) = fields.next() {
                        let (key, value) = field.map_err(epee)?;
                        if key.consume() != b"indices" {
                            continue;
                        }
                        let mut per_tx = value.iterate().map_err(epee)?;
                        while let Some(tx) = per_tx.next() {
                            let mut fields = tx.map_err(epee)?.fields().map_err(epee)?;
                            while let Some(field) = fields.next() {
                                let (key, value) = field.map_err(epee)?;
                                if key.consume() != b"indices" {
                                    continue;
                                }
                                let mut indexes = value.iterate().map_err(epee)?;
                                while let Some(index) = indexes.next() {
                                    output_indexes
                                        .push(index.map_err(epee)?.to_u64().map_err(epee)?);
                                }
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // The output indexes are flat over every transaction of every block;
    // each block needs the index of its first RingCT output.
    let mut indexes = output_indexes.as_slice();
    let mut take = |first: &mut Option<u64>, v1: bool, outputs: usize| {
        if indexes.len() < outputs {
            return Err(invalid("too few output indexes"));
        }
        if !v1 && outputs != 0 {
            *first = first.or(Some(indexes[0]));
        }
        indexes = &indexes[outputs..];
        Ok(())
    };
    let mut out = Vec::with_capacity(blocks.len());
    for (block, transactions) in blocks {
        let mut first = None;
        let miner = block.miner_transaction();
        take(
            &mut first,
            matches!(miner, Transaction::V1 { .. }),
            miner.prefix().outputs.len(),
        )?;
        for (tx, _) in &transactions {
            take(
                &mut first,
                matches!(tx, Transaction::V1 { .. }),
                tx.prefix().outputs.len(),
            )?;
        }
        out.push(RawBlock {
            block,
            transactions,
            first_ringct_output: first,
        });
    }
    if !indexes.is_empty() {
        return Err(invalid("more output indexes than outputs"));
    }
    Ok(out)
}

type ParsedBlock = (Block, Vec<(Transaction<Pruned>, Option<[u8; 32]>)>);

fn parse_block<'a>(entry: EpeeEntry<'a, '_, &'a [u8]>) -> Result<ParsedBlock, SyncError> {
    let mut fields = entry.fields().map_err(epee)?;
    let mut block = None;
    let mut transactions = Vec::new();
    while let Some(field) = fields.next() {
        let (key, value) = field.map_err(epee)?;
        match key.consume() {
            b"block" => {
                let mut bytes = value.to_str().map_err(epee)?.consume();
                block = Some(Block::read(&mut bytes).map_err(|_| invalid("an invalid block"))?);
                if !bytes.is_empty() {
                    return Err(invalid("bytes after a block"));
                }
            }
            b"txs" => {
                let mut entries = value.iterate().map_err(epee)?;
                while let Some(entry) = entries.next() {
                    transactions.push(parse_transaction(entry.map_err(epee)?)?);
                }
            }
            _ => {}
        }
    }
    Ok((
        block.ok_or_else(|| invalid("a block entry without a block"))?,
        transactions,
    ))
}

fn parse_transaction<'a>(
    entry: EpeeEntry<'a, '_, &'a [u8]>,
) -> Result<(Transaction<Pruned>, Option<[u8; 32]>), SyncError> {
    let mut fields = entry.fields().map_err(epee)?;
    let mut tx = None;
    let mut prunable = None;
    while let Some(field) = fields.next() {
        let (key, value) = field.map_err(epee)?;
        match key.consume() {
            b"blob" => {
                let mut bytes = value.to_str().map_err(epee)?.consume();
                tx = Some(
                    Transaction::<Pruned>::read(&mut bytes)
                        .map_err(|_| invalid("an invalid transaction"))?,
                );
                if !bytes.is_empty() {
                    return Err(invalid("bytes after a transaction"));
                }
            }
            b"prunable_hash" => {
                let bytes = value.to_fixed_len_str(32).map_err(epee)?.consume();
                prunable = Some(<[u8; 32]>::try_from(bytes).expect("32 bytes"));
            }
            _ => {}
        }
    }
    let tx = tx.ok_or_else(|| invalid("a transaction entry without a transaction"))?;
    // What can be checked here: version-2 transactions with a real prunable
    // hash; version-2 ones without proofs hash with zeros. Version 1 cannot
    // be checked pruned, and a zero hash is monerod's placeholder.
    let prunable = match &tx {
        Transaction::V1 { .. } => None,
        Transaction::V2 { proofs: None, .. } => Some([0; 32]),
        Transaction::V2 { .. } => prunable.filter(|h| *h != [0; 32]),
    };
    Ok((tx, prunable))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_request_matches_what_monerod_expects() {
        let request = blocks_request(5, 50);
        assert_eq!(&request[..8], &monero_epee::HEADER);
        // Fields as monero-oxide writes them, so nodes treat both the same.
        let mut expected = b"\x01\x11\x01\x01\x01\x01\x02\x01\x01\x0c".to_vec();
        expected.extend(b"\x05prune\x0b\x01");
        expected.extend(b"\x0cstart_height\x05");
        expected.extend(5u64.to_le_bytes());
        expected.extend(b"\x0fmax_block_count\x05");
        expected.extend(50u64.to_le_bytes());
        assert_eq!(request, expected);
    }

    #[test]
    fn malformed_answers_are_errors() {
        assert!(parse_blocks_bin(b"").is_err());
        assert!(parse_blocks_bin(b"not epee at all").is_err());
        let mut no_blocks = monero_epee::HEADER.to_vec();
        no_blocks.extend([monero_epee::VERSION, 0]);
        assert!(parse_blocks_bin(&no_blocks).unwrap().is_empty());
    }
}
