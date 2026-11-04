//! Bounded V3 payouts: up to 16 inputs, one payout and positive internal change.
use super::{
    Checkpoint, PreparedTransaction, WalletNote, decode_address,
    note::{decode_tree, decode_witness, restore_note},
};
use anyhow::{Context, Result, ensure};
use orchard::{
    keys::{FullViewingKey, Scope, SpendAuthorizingKey, SpendingKey},
    note::ExtractedNoteCommitment,
    tree::MerklePath,
};
use rand_core::UnwrapErr;
use std::convert::Infallible;
use zcash_primitives::transaction::{
    TxVersion,
    builder::{BuildConfig, Builder, BundlePadding},
    fees::zip317,
};
use zcash_protocol::{
    consensus::{BlockHeight, BranchId, TEST_NETWORK, TestNetwork},
    memo::MemoBytes,
    value::Zatoshis,
};
use zcash_transparent::builder::TransparentSigningSet;

mod sapling;
/// ZIP317's standard two-action fee, verified against the completed builder below.
pub const FIXED_FEE_ZAT: u64 = 10_000;
pub const MAX_INPUTS: usize = 16;

pub fn fee_for_inputs(count: usize) -> Result<u64> {
    ensure!(
        (1..=MAX_INPUTS).contains(&count),
        "unsupported payout input count"
    );
    Ok(count.max(2) as u64 * 5_000)
}

fn payout_builder(
    fvk: &FullViewingKey,
    checkpoint: &Checkpoint,
    inputs: &[WalletNote],
    destination: &str,
    amount: u64,
) -> Result<Builder<TestNetwork, ()>> {
    ensure!(amount > 0, "payout must be positive");
    let destination = decode_address(destination)?;
    let amount = Zatoshis::from_u64(amount).context("payout value out of range")?;
    let fee = fee_for_inputs(inputs.len())?;
    let total = inputs.iter().try_fold(0u64, |sum, input| {
        sum.checked_add(input.value).context("input value overflow")
    })?;
    let change = total
        .checked_sub(amount.into_u64())
        .and_then(|v| v.checked_sub(fee))
        .context("insufficient payout liquidity")?;
    ensure!(change > 0, "payout requires positive internal change");
    let change = Zatoshis::from_u64(change).context("change value out of range")?;
    let target = checkpoint
        .height
        .checked_add(1)
        .context("target height overflow")?;
    let expiry = target.checked_add(120).context("expiry height overflow")?;
    ensure!(expiry < 500_000_000, "unsupported expiry height");
    for height in [checkpoint.height, target, expiry] {
        ensure!(
            BranchId::for_height(&TEST_NETWORK, BlockHeight::from_u32(height)) == BranchId::Nu7,
            "unsupported consensus branch"
        );
    }
    let tree = decode_tree(&checkpoint.frontier)?;
    let anchor = tree.root().into();
    let mut spends = Vec::with_capacity(inputs.len());
    for (index, input) in inputs.iter().enumerate() {
        ensure!(
            !inputs[..index]
                .iter()
                .any(|n| n.id == input.id || n.nullifier == input.nullifier),
            "duplicate payout input"
        );
        ensure!(input.spent_height.is_none(), "input already spent");
        ensure!(
            input.mined_height <= checkpoint.height,
            "input is newer than checkpoint"
        );
        let note = restore_note(input, fvk)?;
        let witness = decode_witness(&input.witness)?;
        ensure!(
            u64::from(witness.tip_position()).checked_add(1) == Some(tree.size() as u64),
            "input witness checkpoint size mismatch"
        );
        ensure!(
            u64::from(witness.witnessed_position()) == input.position,
            "input position mismatch"
        );
        let path = MerklePath::from(witness.path().context("incomplete input witness")?);
        ensure!(
            path.root(ExtractedNoteCommitment::from(note.commitment())) == anchor
                && witness.root() == tree.root(),
            "input witness root mismatch"
        );
        spends.push((note, path));
    }
    let mut builder = Builder::new(
        TEST_NETWORK,
        BlockHeight::from_u32(target),
        BuildConfig::Standard {
            sapling_anchor: None,
            orchard_anchor: None,
            ironwood_anchor: Some(anchor),
            orchard_padding: BundlePadding::DEFAULT,
            ironwood_padding: BundlePadding::DEFAULT,
        },
    )
    .with_expiry_height(BlockHeight::from_u32(expiry));
    for (note, path) in spends {
        builder.add_ironwood_spend::<Infallible>(fvk.clone(), note, path)?;
    }
    builder.add_ironwood_output::<Infallible>(
        Some(fvk.to_ovk(Scope::External)),
        destination,
        amount,
        MemoBytes::empty(),
    )?;
    builder.add_ironwood_output::<Infallible>(
        Some(fvk.to_ovk(Scope::Internal)),
        fvk.address_at(0_u32, Scope::Internal),
        change,
        MemoBytes::empty(),
    )?;
    let built_fee = builder
        .get_fee(&zip317::FeeRule::standard())
        .map_err(|_| anyhow::anyhow!("ZIP317 fee computation failed"))?;
    ensure!(built_fee.into_u64() == fee, "unsupported transaction fee");
    Ok(builder)
}

pub(super) fn sign(
    key: &SpendingKey,
    checkpoint: &Checkpoint,
    inputs: &[WalletNote],
    destination: &str,
    amount: u64,
) -> Result<PreparedTransaction> {
    let builder = payout_builder(
        &FullViewingKey::from(key),
        checkpoint,
        inputs,
        destination,
        amount,
    )?;
    let result = builder
        .build(
            &TransparentSigningSet::new(),
            &[],
            &[SpendAuthorizingKey::from(key)],
            UnwrapErr(getrandom::SysRng),
            &sapling::DisabledSapling,
            &sapling::DisabledSapling,
            &zip317::FeeRule::standard(),
        )
        .map_err(|_| anyhow::anyhow!("Ironwood transaction proving/signing failed"))?;
    let tx = result.transaction();
    ensure!(
        tx.version() == TxVersion::V6 && tx.consensus_branch_id() == BranchId::Nu7,
        "unexpected signed transaction version"
    );
    let expiry_height = u32::from(tx.expiry_height());
    ensure!(expiry_height != 0, "zero transaction expiry");
    let mut bytes = vec![];
    tx.write(&mut bytes)?;
    Ok(PreparedTransaction {
        bytes,
        txid: *tx.txid().as_ref(),
        inputs: inputs.iter().map(|n| n.id).collect(),
        expiry_height,
        fee: fee_for_inputs(inputs.len())?,
    })
}

#[cfg(test)]
#[path = "transaction_tests.rs"]
mod tests;
