//! Read-only capability/positive-history probe; never death evidence.
//! cargo run --locked --offline -p zcash --example payout_history_probe
//! Optional arguments: START END DISPLAY_TXID INCLUSION_HEIGHT (at most 16 blocks).
use std::{collections::HashSet, io::Cursor};

use anyhow::{Context, Result, ensure};
use incrementalmerkletree::frontier::CommitmentTree;
use orchard::{note::ExtractedNoteCommitment, tree::MerkleHashOrchard};
use zcash::rpc::{CompactBlock, LightwalletClient};
use zcash_primitives::{merkle_tree::read_commitment_tree, transaction::Transaction};
use zcash_protocol::consensus::{BlockHeight, BranchId, TEST_NETWORK};

type Tree = CommitmentTree<MerkleHashOrchard, 32>;
const ENDPOINT: &str = "https://testnet.zec.rocks:443";
const PAID: &str = "c1636ebba7de300936ae0580b32963bbdaa0a6d8cfb4282e79162bdcb152423a";

fn fixed(bytes: &[u8]) -> Result<[u8; 32]> {
    bytes.try_into().context("invalid 32-byte chain field")
}

async fn tree(client: &LightwalletClient, block: &CompactBlock) -> Result<Tree> {
    let state = client.tree(u32::try_from(block.height)?).await?;
    ensure!(
        state.network == "test" && state.height == block.height,
        "wrong tree identity"
    );
    let mut hash = hex::decode(state.hash)?;
    hash.reverse();
    ensure!(hash == block.hash, "tree/compact hash mismatch");
    ensure!(state.ironwood_tree.len() <= 2200, "oversized tree");
    let bytes = hex::decode(state.ironwood_tree)?;
    let mut reader = Cursor::new(bytes.as_slice());
    let tree = read_commitment_tree(&mut reader)?;
    ensure!(
        reader.position() == bytes.len() as u64,
        "trailing tree bytes"
    );
    ensure!(
        tree.size() as u64
            == u64::from(
                block
                    .chain_metadata
                    .as_ref()
                    .context("missing chain metadata")?
                    .ironwood_commitment_tree_size
            ),
        "tree size mismatch"
    );
    Ok(tree)
}

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<_> = std::env::args().skip(1).collect();
    ensure!(
        args.is_empty() || args.len() == 4 || args == ["--activation"],
        "usage: payout_history_probe [--activation | START END DISPLAY_TXID INCLUSION_HEIGHT]"
    );
    let activation = args == ["--activation"];
    let (start, end, display, inclusion) = if activation {
        (4_465_026, 4_465_026, PAID, 4_465_026)
    } else if args.is_empty() {
        (4_544_278, 4_544_280, PAID, 4_544_279)
    } else {
        (
            args[0].parse::<u32>()?,
            args[1].parse::<u32>()?,
            args[2].as_str(),
            args[3].parse::<u32>()?,
        )
    };
    ensure!(
        start >= 4_465_026 && end >= start && end - start < 16,
        "range must contain 1..16 NU7 blocks"
    );
    ensure!(
        (start..=end).contains(&inclusion),
        "inclusion outside range"
    );
    let mut expected = fixed(&hex::decode(display)?)?;
    expected.reverse();
    println!("read_only=true death_evidence=false endpoint={ENDPOINT}");
    let client = LightwalletClient::connect(ENDPOINT).await?;
    let tip = client.tip().await?;
    ensure!(tip >= end, "tip below requested range");
    let tip_hash = client.block_hash(tip).await?;
    let mut previous = client.block(start - 1).await?;
    fixed(&previous.hash)?;
    let baseline_hash = previous.hash.clone();
    let mut frontier = tree(&client, &previous).await?;
    let mut checkpoints = Vec::new();
    let mut found = 0;
    for height in start..=end {
        let block = client.block(height).await?;
        fixed(&block.hash)?;
        ensure!(
            block.prev_hash == previous.hash,
            "nonconsecutive block parent"
        );
        let mut txids = HashSet::new();
        let mut last = None;
        let mut actions = 0_u64;
        for tx in &block.vtx {
            ensure!(
                last.is_none_or(|index| tx.index > index),
                "duplicate/unordered tx index"
            );
            last = Some(tx.index);
            let txid = fixed(&tx.txid)?;
            ensure!(txids.insert(txid), "duplicate compact txid");
            for action in &tx.ironwood_actions {
                fixed(&action.nullifier)?;
                fixed(&action.ephemeral_key)?;
                ensure!(
                    action.ciphertext.len() == 52,
                    "invalid compact ciphertext length"
                );
                let cmx = Option::<ExtractedNoteCommitment>::from(
                    ExtractedNoteCommitment::from_bytes(&fixed(&action.cmx)?),
                )
                .context("noncanonical commitment")?;
                frontier
                    .append(MerkleHashOrchard::from_cmx(&cmx))
                    .map_err(|_| anyhow::anyhow!("tree full"))?;
                actions += 1;
            }
            if !activation && txid == expected {
                ensure!(
                    height == inclusion && !tx.ironwood_actions.is_empty(),
                    "wrong known inclusion/bundle"
                );
                let raw = client.transaction(expected).await?;
                ensure!(
                    raw.height == u64::from(height)
                        && !raw.data.is_empty()
                        && raw.data.len() <= 2_000_000,
                    "invalid raw transaction"
                );
                let mut reader = Cursor::new(&raw.data);
                let full = Transaction::read(
                    &mut reader,
                    BranchId::for_height(&TEST_NETWORK, BlockHeight::from_u32(height)),
                )?;
                ensure!(
                    reader.position() == raw.data.len() as u64 && full.txid().as_ref() == &expected,
                    "full transaction identity mismatch"
                );
                ensure!(
                    full.version() == zcash_primitives::transaction::TxVersion::V6
                        && full.consensus_branch_id() == BranchId::Nu7,
                    "unexpected full transaction format"
                );
                let bundle = full
                    .ironwood_bundle()
                    .context("missing full Ironwood bundle")?;
                ensure!(
                    bundle.actions().len() == tx.ironwood_actions.len(),
                    "compact/full action count mismatch"
                );
                for (full, compact) in bundle.actions().iter().zip(&tx.ironwood_actions) {
                    ensure!(
                        full.nullifier().to_bytes().as_slice() == compact.nullifier
                            && full.cmx().to_bytes().as_slice() == compact.cmx
                            && full.encrypted_note().epk_bytes.as_slice() == compact.ephemeral_key
                            && full.encrypted_note().enc_ciphertext.0[..52] == compact.ciphertext,
                        "compact/full action mismatch"
                    );
                }
                found += 1;
                println!(
                    "known_paid_height={height} full_txid_match=true v6=true nu7=true ironwood_actions={} expiry={}",
                    tx.ironwood_actions.len(),
                    u32::from(full.expiry_height())
                );
            }
        }
        let expected_tree = tree(&client, &block).await?;
        ensure!(
            frontier.size() == expected_tree.size() && frontier.root() == expected_tree.root(),
            "enumerated commitments disagree with tree"
        );
        println!(
            "height={height} compact_transactions={} ironwood_actions={actions} linked=true tree_match=true",
            block.vtx.len()
        );
        checkpoints.push((height, block.hash.clone()));
        previous = block;
    }
    ensure!(
        activation || found == 1,
        "known paid transaction not found exactly once"
    );
    ensure!(
        client.block(start - 1).await?.hash == baseline_hash,
        "baseline changed"
    );
    for (height, hash) in checkpoints {
        ensure!(
            client.block(height).await?.hash == hash,
            "historical checkpoint changed"
        );
    }
    ensure!(
        client.tip().await? >= tip && client.block_hash(tip).await? == tip_hash,
        "tip checkpoint changed"
    );
    println!(
        "tip={tip} checkpoint_rechecked=true activation_probe={activation} history_capability_probe=pass death_evidence=false"
    );
    Ok(())
}
