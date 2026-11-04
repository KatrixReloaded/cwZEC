//! Selection uses application-filtered state. PostgreSQL reservations and the
//! durable signed-transaction journal belong to the application.
use crate::{
    core::{PreparedTransaction, WalletNote, WalletState},
    rpc::LightwalletClient,
};
use anyhow::{Context, Result, ensure};
use std::{collections::HashSet, io::Cursor};
use zcash_primitives::transaction::{Transaction, TxVersion};
use zcash_protocol::{consensus::BranchId, value::Zatoshis};

/// ZIP317 fee policy for bounded payouts with two outputs.
pub use crate::core::transaction::{FIXED_FEE_ZAT, MAX_INPUTS, fee_for_inputs};

/// The application must filter source/deposit authorization before calling this,
/// then reserve the returned input atomically before signing. Reserved nullifiers
/// include all historical attempts, including those absent after a chain rewind.
pub fn select_input<'a>(
    state: &'a WalletState,
    reserved: &HashSet<[u8; 32]>,
    confirmations: u32,
    amount: u64,
) -> Result<&'a WalletNote> {
    ensure!(confirmations > 0, "confirmation policy must be positive");
    ensure!(amount > 0, "payout must be positive");
    Zatoshis::from_u64(amount).context("payout value out of range")?;
    let minimum = amount
        .checked_add(FIXED_FEE_ZAT)
        .and_then(|v| v.checked_add(1))
        .context("payout arithmetic overflow")?;
    state.notes.iter().filter(|note| {
        note.spent_height.is_none() && !reserved.contains(&note.nullifier)
            && state.checkpoint.height.checked_sub(note.mined_height).and_then(|n| n.checked_add(1)).is_some_and(|n| n >= confirmations)
            && note.value >= minimum && Zatoshis::from_u64(note.value).is_ok()
    }).min_by_key(|note| (note.value, note.position))
        .context("insufficient confirmed unreserved single-note liquidity; fragmented liquidity is unsupported")
}

/// Prefer one note; otherwise combine the largest available notes with the fewest
/// inputs. The fee grows with input count and is checked again by the builder.
pub fn select_inputs<'a>(
    state: &'a WalletState,
    reserved: &HashSet<[u8; 32]>,
    confirmations: u32,
    amount: u64,
) -> Result<Vec<&'a WalletNote>> {
    ensure!(
        confirmations > 0 && amount > 0,
        "positive payout and confirmations required"
    );
    Zatoshis::from_u64(amount).context("payout value out of range")?;
    if let Ok(input) = select_input(state, reserved, confirmations, amount) {
        return Ok(vec![input]);
    }
    let mut notes: Vec<_> = state
        .notes
        .iter()
        .filter(|n| {
            n.spent_height.is_none()
                && !reserved.contains(&n.nullifier)
                && state
                    .checkpoint
                    .height
                    .checked_sub(n.mined_height)
                    .and_then(|h| h.checked_add(1))
                    .is_some_and(|c| c >= confirmations)
                && Zatoshis::from_u64(n.value).is_ok()
        })
        .collect();
    notes.sort_by_key(|n| (std::cmp::Reverse(n.value), n.position));
    let mut total = 0u64;
    for count in 1..=notes.len().min(MAX_INPUTS) {
        total = total
            .checked_add(notes[count - 1].value)
            .context("input value overflow")?;
        if total
            > amount
                .checked_add(fee_for_inputs(count)?)
                .context("payout arithmetic overflow")?
        {
            notes.truncate(count);
            return Ok(notes);
        }
    }
    anyhow::bail!("insufficient confirmed unreserved liquidity within payout input limit")
}

fn validate_prepared(prepared: &PreparedTransaction) -> Result<()> {
    ensure!(
        !prepared.bytes.is_empty() && prepared.bytes.len() <= 2_000_000,
        "invalid transaction size"
    );
    let mut cursor = Cursor::new(&prepared.bytes);
    let tx = Transaction::read(&mut cursor, BranchId::Nu7).context("decode stored transaction")?;
    ensure!(
        cursor.position() == prepared.bytes.len() as u64,
        "trailing transaction bytes"
    );
    ensure!(
        tx.version() == TxVersion::V6 && tx.consensus_branch_id() == BranchId::Nu7,
        "unsupported stored transaction"
    );
    ensure!(
        tx.txid().as_ref() == &prepared.txid,
        "stored transaction id mismatch"
    );
    ensure!(
        prepared.expiry_height != 0
            && prepared.expiry_height < 500_000_000
            && u32::from(tx.expiry_height()) == prepared.expiry_height,
        "stored transaction expiry mismatch"
    );
    ensure!(
        prepared.fee == fee_for_inputs(prepared.inputs.len())?
            && prepared
                .inputs
                .iter()
                .enumerate()
                .all(|(i, id)| !prepared.inputs[..i].contains(id)),
        "unsupported stored payout metadata"
    );
    ensure!(
        tx.transparent_bundle().is_none()
            && tx.sapling_bundle().is_none()
            && tx.orchard_bundle().is_none()
            && tx.sprout_bundle().is_none(),
        "unsupported stored transaction pool"
    );
    let bundle = tx
        .ironwood_bundle()
        .context("stored payout lacks Ironwood bundle")?;
    ensure!(
        bundle.actions().len() == prepared.inputs.len().max(2)
            && i64::from(*bundle.value_balance()) == prepared.fee as i64,
        "stored payout fee/action mismatch"
    );
    Ok(())
}

/// Call only after the application commits signed bytes and all metadata to its
/// durable outbound journal. Failure never authorizes a replacement transaction.
pub async fn broadcast(client: &LightwalletClient, prepared: &PreparedTransaction) -> Result<()> {
    validate_prepared(prepared)?;
    client.submit(&prepared.bytes).await
}

#[cfg(test)]
pub(crate) mod tests;
