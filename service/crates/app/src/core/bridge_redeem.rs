//! Bounded burn reconciliation and payment/fulfillment advancement.
use super::{
    Service, bridge::decrypt as decrypt_actual, bridge_mint::MintFees, bridge_types::*,
    destination::DestinationKey, redeem_types::*, zama::ZamaService,
};
use crate::store::Store;
use alloy::signers::local::PrivateKeySigner;
use anyhow::{Context, Result, ensure};
use std::{
    collections::HashSet,
    sync::{Arc, atomic::Ordering},
};
use zama::{contract::RedemptionStatus, fhe, watcher::Event};
use zcash::vault;
use zeroize::Zeroizing;

pub struct BridgeRedeemer {
    store: Store,
    zama: ZamaService,
    sidecar: fhe::Client,
    wallet: Arc<Service>,
}

impl BridgeRedeemer {
    pub async fn new(
        mut store: Store,
        zama: ZamaService,
        sidecar: fhe::Client,
        wallet: Arc<Service>,
    ) -> Result<Self> {
        store
            .initialize_bridge(BridgeDomain {
                chain_id: zama.client().chain_id(),
                contract: zama.client().address(),
            })
            .await?;
        Ok(Self {
            store,
            zama,
            sidecar,
            wallet,
        })
    }

    /// Run before reconciliation/accounting/signing on every coordinator pass.
    pub async fn collect_death_once(&mut self) -> Result<bool> {
        let wallet = self.wallet.clone();
        let _payment = wallet.payment_lock.lock().await;
        super::payout_death::collect_once(&mut self.store, wallet.watcher.client()).await
    }

    /// Both scanners run before this method. Record burns and real payment effects
    /// before the coordinator computes accounting at this same pair.
    pub async fn reconcile_at(
        &mut self,
        pair: BridgeCheckpointPair,
        signer: &PrivateKeySigner,
        auditor: &PrivateKeySigner,
        key: &DestinationKey,
    ) -> Result<()> {
        let wallet = self.wallet.clone();
        let _payment = wallet.payment_lock.lock().await;
        super::payout_death::revalidate_acted(&mut self.store, wallet.watcher.client()).await?;
        self.check_pair(pair).await?;
        let events = self.zama.events().await?;
        let known = self.store.bridge_redeems().await?;
        for event in events
            .iter()
            .filter(|e| e.block.number <= pair.checkpoint.number)
        {
            let Event::RedeemRequested { redeem_id, .. } = &event.event else {
                continue;
            };
            let (actual, destination) = read_burn(
                self.zama.client(),
                &self.sidecar,
                signer,
                key,
                event,
                pair.checkpoint,
                known.iter().find(|r| r.id == *redeem_id),
            )
            .await?;
            self.check_pair(pair).await?;
            self.store
                .record_bridge_redeem(
                    event,
                    pair,
                    actual,
                    destination.as_deref().map(|d| d.as_str()),
                )
                .await?;
        }
        for redeem in self
            .store
            .bridge_redeems()
            .await?
            .into_iter()
            .filter(|r| r.canonical)
        {
            self.check_pair(pair).await?;
            self.store.settle_bridge_payment(redeem.id, pair).await?;
        }
        for event in events
            .iter()
            .filter(|e| e.block.number <= pair.checkpoint.number)
        {
            if let Event::RedeemFulfilled { redeem_id, .. } = event.event {
                self.check_pair(pair).await?;
                self.store
                    .reconcile_bridge_fulfillment(redeem_id, event, pair)
                    .await?;
            }
        }
        for event in events
            .iter()
            .filter(|e| e.block.number <= pair.checkpoint.number)
        {
            if let Event::RedeemRecredited { redeem_id, .. } = event.event {
                let redeem = self
                    .store
                    .bridge_redeems()
                    .await?
                    .into_iter()
                    .find(|r| r.id == redeem_id)
                    .context("unknown recredited burn")?;
                let actual = if redeem.state == RedeemState::Recredited {
                    // The original verified restoration is durable. Revalidate its
                    // exact event and own transaction below without another KMS call.
                    redeem.actual
                } else {
                    verify_restoration(
                        self.zama.client(),
                        &self.sidecar,
                        auditor,
                        &redeem,
                        event,
                        &events,
                    )
                    .await?
                };
                self.check_pair(pair).await?;
                super::payout_death::revalidate_acted(&mut self.store, wallet.watcher.client())
                    .await?;
                self.store
                    .reconcile_bridge_recredit(redeem_id, event, pair, actual)
                    .await?;
            }
        }
        Ok(())
    }

    /// Called only by the authenticated trusted-operator authorization route.
    pub async fn authorize_recredit(
        &mut self,
        id: alloy::primitives::U256,
        version: u64,
    ) -> Result<BridgeRedeem> {
        let wallet = self.wallet.clone();
        let _payment = wallet.payment_lock.lock().await;
        let pair = self.store.bridge_checkpoint_pair().await?;
        self.check_pair(pair).await?;
        super::payout_death::authorize_recredit(
            &mut self.store,
            wallet.watcher.client(),
            id,
            version,
            pair,
        )
        .await
    }

    /// Perform at most one payment or fulfillment action, recovering all existing
    /// signed identities before considering a new payment.
    pub async fn advance_once(
        &mut self,
        accounting_id: i64,
        signer: &PrivateKeySigner,
        fees: MintFees,
    ) -> Result<bool> {
        let wallet = self.wallet.clone();
        let _payment = wallet.payment_lock.lock().await;
        super::payout_death::revalidate_acted(&mut self.store, wallet.watcher.client())
            .await
            .context("redemption: revalidate recovery")?;
        let pair = self
            .store
            .bridge_checkpoint_pair()
            .await
            .context("redemption: checkpoint")?;
        self.check_pair(pair)
            .await
            .context("redemption: validate checkpoint")?;
        let redeems = self.store.bridge_redeems().await?;
        if let Some(redeem) = redeems
            .iter()
            .find(|r| r.state == RedeemState::RecreditPending)
        {
            self.recredit(redeem, pair, signer, fees)
                .await
                .context("redemption: recredit")?;
            return Ok(true);
        }
        let mut issued = HashSet::new();
        for redeem in redeems.iter().filter(|r| r.state == RedeemState::Reserved) {
            if self
                .wallet
                .store
                .lock()
                .await
                .get_attempt(&redeem.operation_id)
                .await?
                .is_some_and(|a| a.prepared.is_some())
            {
                issued.insert(redeem.id);
                if self
                    .recover_payment(redeem, pair)
                    .await
                    .context("redemption: recover payout")?
                {
                    return Ok(true);
                }
            }
        }
        for redeem in redeems
            .iter()
            .filter(|r| matches!(r.state, RedeemState::Paid | RedeemState::Fulfilled))
        {
            if self
                .zama
                .transaction(&redeem.fulfillment_id())
                .await?
                .is_some()
            {
                if redeem.state == RedeemState::Fulfilled {
                    continue;
                }
                self.fulfill(redeem, pair, signer, fees)
                    .await
                    .context("redemption: fulfill")?;
                return Ok(true);
            }
        }
        if let Some(redeem) = redeems.iter().find(|r| r.state == RedeemState::Paid) {
            self.fulfill(redeem, pair, signer, fees)
                .await
                .context("redemption: fulfill")?;
            return Ok(true);
        }
        let Some(redeem) = redeems
            .iter()
            .find(|r| r.state == RedeemState::Reserved && !issued.contains(&r.id))
            .or_else(|| redeems.iter().find(|r| r.state == RedeemState::Ready))
        else {
            return Ok(false);
        };
        ensure!(
            redeem.canonical,
            "noncanonical burn cannot authorize payment"
        );
        let mut store = self.wallet.store.lock().await;
        let mut state = store.load().await?;
        let amount = redeem
            .actual
            .checked_sub(REDEEM_FEE_ZAT)
            .filter(|a| *a >= MIN_PAYOUT_ZAT)
            .context("invalid payout amount")?;
        let inputs: Vec<_> = if redeem.state == RedeemState::Reserved {
            let attempt = store
                .get_attempt(&redeem.operation_id)
                .await?
                .context("reserved payment journal missing")?;
            ensure!(
                (1..=vault::MAX_INPUTS).contains(&attempt.inputs.len())
                    && attempt.prepared.is_none()
                    && attempt.amount == amount
                    && Some(&attempt.destination) == redeem.destination.as_ref(),
                "reserved payment intent mismatch"
            );
            attempt
                .inputs
                .iter()
                .map(|id| {
                    state
                        .notes
                        .iter()
                        .find(|n| n.id == *id && n.spent_height.is_none())
                        .cloned()
                        .context("reserved input unavailable; reservation retained")
                })
                .collect::<Result<Vec<_>>>()?
        } else {
            state.notes.retain(|n| n.mined_height <= pair.zcash_height);
            let reserved: HashSet<_> = store
                .reserved_inputs()
                .await?
                .into_iter()
                .map(|(_, nf)| nf)
                .collect();
            vault::select_inputs(&state, &reserved, 3, amount)
                .context("redemption: select payout input")?
                .into_iter()
                .cloned()
                .collect()
        };
        drop(store);
        self.check_pair(pair)
            .await
            .context("redemption: validate checkpoint")?;
        if redeem.state == RedeemState::Ready {
            self.store
                .reserve_bridge_redeem_inputs(
                    redeem.id,
                    redeem.version,
                    accounting_id,
                    &inputs.iter().map(|n| n.id).collect::<Vec<_>>(),
                )
                .await
                .context("redemption: reserve payout input")?;
        }
        let destination = redeem.destination.clone().context("destination missing")?;
        let anchor = state.checkpoint;
        let expected_height = anchor.height;
        let expected_hash = anchor.hash;
        let wallet_signer = self.wallet.signer.clone();
        self.store
            .check_payment_accounting()
            .await
            .context("redemption: payment accounting")?;
        let prepared = tokio::task::spawn_blocking(move || {
            wallet_signer.sign_inputs(&anchor, &inputs, &destination, amount)
        })
        .await
        .context("redemption: proving task")?
        .context("redemption: sign payout")?;
        self.check_pair(pair)
            .await
            .context("redemption: validate checkpoint")?;
        let mut store = self.wallet.store.lock().await;
        ensure!(
            store
                .checkpoint(expected_height)
                .await?
                .is_some_and(|c| c.hash == expected_hash),
            "payment anchor reorganized"
        );
        ensure!(
            store.load().await?.checkpoint.height < prepared.expiry_height,
            "payment expired while signing; reservation retained"
        );
        store.save_prepared(&redeem.operation_id, &prepared).await?;
        drop(store);
        super::payout::interrupt("payout-journal", &prepared.bytes);
        self.recover_payment(redeem, pair)
            .await
            .context("redemption: recover payout")?;
        Ok(true)
    }

    async fn recover_payment(
        &mut self,
        redeem: &BridgeRedeem,
        pair: BridgeCheckpointPair,
    ) -> Result<bool> {
        self.check_pair(pair).await?;
        ensure!(redeem.canonical, "payment burn is not canonical");
        let attempt = self
            .wallet
            .store
            .lock()
            .await
            .get_attempt(&redeem.operation_id)
            .await?
            .context("payment journal missing")?;
        ensure!(
            Some(&attempt.destination) == redeem.destination.as_ref()
                && attempt.amount == redeem.actual - REDEEM_FEE_ZAT,
            "saved payment intent mismatch"
        );
        if attempt.observed_height.is_some() {
            return Ok(false);
        }
        let prepared = attempt.prepared.context("payment is not prepared")?;
        if self.wallet.watcher.client().tip().await? >= prepared.expiry_height {
            return Ok(false);
        }
        self.check_pair(pair).await?;
        if vault::broadcast(self.wallet.watcher.client(), &prepared)
            .await
            .is_ok()
        {
            super::payout::interrupt("payout-broadcast", &prepared.bytes);
            self.wallet
                .store
                .lock()
                .await
                .mark_broadcast(&redeem.operation_id)
                .await?;
        }
        Ok(true)
    }

    async fn fulfill(
        &mut self,
        redeem: &BridgeRedeem,
        pair: BridgeCheckpointPair,
        signer: &PrivateKeySigner,
        fees: MintFees,
    ) -> Result<()> {
        self.check_pair(pair).await?;
        let txid = self
            .store
            .settle_bridge_payment(redeem.id, pair)
            .await?
            .context("fulfillment lacks confirmed real payment")?;
        let call = self.zama.client().domain().fulfill(redeem.id, txid);
        let id = redeem.fulfillment_id();
        if let Some(saved) = self.zama.transaction(&id).await? {
            ensure!(
                saved.signed.sender() == signer.address()
                    && saved.signed.fulfillment_identity() == Some((redeem.id, txid)),
                "saved fulfillment identity mismatch"
            );
        } else {
            super::payout::interrupt("payment-confirmed", txid.as_slice());
            ensure!(
                fees.gas_limit > 0
                    && fees.max_fee_per_gas > 0
                    && fees.max_priority_fee_per_gas <= fees.max_fee_per_gas,
                "invalid fulfillment fees"
            );
            self.zama
                .prepare_operator(
                    &id,
                    signer,
                    &call,
                    fees.gas_limit,
                    fees.max_fee_per_gas,
                    fees.max_priority_fee_per_gas,
                )
                .await?;
        }
        self.zama.recover_operator(&id).await?;
        Ok(())
    }

    async fn recredit(
        &mut self,
        redeem: &BridgeRedeem,
        pair: BridgeCheckpointPair,
        signer: &PrivateKeySigner,
        fees: MintFees,
    ) -> Result<()> {
        self.check_pair(pair).await?;
        ensure!(
            redeem.canonical && redeem.actual > 0,
            "canonical nonzero burn required"
        );
        super::payout_death::revalidate_acted(&mut self.store, self.wallet.watcher.client())
            .await?;
        let issued = self.store.payout_history(redeem.id).await?.is_some();
        let call = self.zama.client().domain().recredit(redeem.id);
        let id = redeem.recredit_id();
        if let Some(saved) = self.zama.transaction(&id).await? {
            ensure!(
                saved.signed.sender() == signer.address()
                    && saved.signed.matches_call(&call)
                    && saved.signed.recredit_identity() == Some(redeem.id),
                "saved recredit identity mismatch"
            );
        } else {
            let record = self
                .zama
                .client()
                .redemption_at(redeem.id, pair.checkpoint)
                .await?;
            ensure!(
                record.user == redeem.burner
                    && record.amount == redeem.handle
                    && record.status == RedemptionStatus::Pending
                    && record.zcash_txid.is_zero(),
                "recredit requires original pending burn"
            );
            ensure!(
                fees.gas_limit > 0
                    && fees.max_fee_per_gas > 0
                    && fees.max_priority_fee_per_gas <= fees.max_fee_per_gas,
                "invalid recredit fees"
            );
            super::payout_death::revalidate_acted(&mut self.store, self.wallet.watcher.client())
                .await?;
            self.check_pair(pair).await?;
            if issued {
                self.zama
                    .prepare_operator_after_death(
                        &id,
                        signer,
                        &call,
                        fees.gas_limit,
                        fees.max_fee_per_gas,
                        fees.max_priority_fee_per_gas,
                        self.wallet.watcher.client(),
                    )
                    .await?;
            } else {
                self.zama
                    .prepare_operator(
                        &id,
                        signer,
                        &call,
                        fees.gas_limit,
                        fees.max_fee_per_gas,
                        fees.max_priority_fee_per_gas,
                    )
                    .await?;
            }
        }
        super::payout_death::revalidate_acted(&mut self.store, self.wallet.watcher.client())
            .await?;
        if issued {
            self.zama
                .recover_operator_after_death(&id, self.wallet.watcher.client())
                .await?;
        } else {
            self.zama.recover_operator(&id).await?;
        }
        Ok(())
    }

    async fn check_pair(&mut self, pair: BridgeCheckpointPair) -> Result<()> {
        ensure!(
            self.wallet.healthy.load(Ordering::Acquire),
            "wallet scanner unavailable"
        );
        ensure!(
            self.store.bridge_checkpoint_pair().await? == pair,
            "checkpoint pair changed; reconcile and account again"
        );
        self.zama.client().check_canonical(pair.checkpoint).await?;
        ensure!(
            self.zama.latest_height().await? > pair.checkpoint.number,
            "EVM checkpoint needs two confirmations"
        );
        ensure!(
            u64::from(self.wallet.watcher.client().tip().await?)
                >= u64::from(pair.zcash_height) + 2,
            "Zcash checkpoint needs three confirmations"
        );
        ensure!(
            self.wallet
                .watcher
                .client()
                .block_hash(pair.zcash_height)
                .await?
                == pair.zcash_hash,
            "Zcash checkpoint is not canonical"
        );
        Ok(())
    }
}

async fn read_burn(
    contract: &zama::contract::CwzecClient,
    sidecar: &fhe::Client,
    signer: &PrivateKeySigner,
    key: &DestinationKey,
    event: &zama::watcher::EventObservation,
    checkpoint: zama::watcher::BlockRef,
    known: Option<&BridgeRedeem>,
) -> Result<(u64, Option<Zeroizing<String>>)> {
    let Event::RedeemRequested {
        redeem_id,
        user,
        amount,
        dest_ct,
    } = &event.event
    else {
        anyhow::bail!("burn event required")
    };
    ensure!(
        event.block.number <= checkpoint.number,
        "burn after checkpoint"
    );
    let redemption = contract.redemption_at(*redeem_id, checkpoint).await?;
    ensure!(
        redemption.user == *user && redemption.amount == *amount && !amount.is_zero(),
        "burn identity differs from canonical contract state"
    );
    ensure!(
        matches!(
            redemption.status,
            RedemptionStatus::Pending | RedemptionStatus::Fulfilled
        ) || (redemption.status == RedemptionStatus::Recredited
            && known.is_some_and(|r| r.canonical
                && matches!(
                    r.state,
                    RedeemState::RecreditPending | RedeemState::Recredited
                ))),
        "unsupported or missing redemption state"
    );
    ensure!(
        (redemption.status != RedemptionStatus::Fulfilled) == redemption.zcash_txid.is_zero(),
        "inconsistent redemption fulfillment state"
    );
    if let Some(saved) = known.filter(|saved| {
        saved.canonical
            && saved.source == event.block
            && saved.source_tx == event.transaction_hash
            && saved.source_log == event.log_index
    }) {
        ensure!(
            saved.id == *redeem_id
                && saved.burner == *user
                && saved.handle == *amount
                && saved.dest_ct == *dest_ct,
            "immutable burn evidence conflict"
        );
        return Ok((saved.actual, saved.destination.clone().map(Zeroizing::new)));
    }
    // Transport/KMS/permit failures propagate, never become actual zero.
    let actual = decrypt_actual(contract, sidecar, *amount, signer).await?;
    // Destination authentication failure is rejected only after actual burn is known.
    let destination = key
        .decrypt_destination(contract.chain_id(), contract.address(), *user, dest_ct)
        .ok();
    Ok((actual, destination))
}

/// The recredit event ciphertext belongs to the burner. Verify the exact supply
/// increase with the auditor instead, accepting only an isolated supply mutation.
async fn verify_restoration(
    contract: &zama::contract::CwzecClient,
    sidecar: &fhe::Client,
    auditor: &PrivateKeySigner,
    redeem: &BridgeRedeem,
    event: &zama::watcher::EventObservation,
    events: &[zama::watcher::EventObservation],
) -> Result<u64> {
    let Event::RedeemRecredited {
        redeem_id,
        user,
        amount,
    } = event.event
    else {
        anyhow::bail!("recredit event required")
    };
    ensure!(
        redeem.id == redeem_id
            && redeem.burner == user
            && !amount.is_zero()
            && redeem.actual > 0
            && redeem.canonical
            && matches!(
                redeem.state,
                RedeemState::RecreditPending | RedeemState::Recredited
            ),
        "unauthorized restoration evidence"
    );
    // Scanned blocks contain all contract events. Reject another mint, burn or
    // recredit, even one whose confidential actual value could be zero.
    ensure!(
        events
            .iter()
            .filter(|e| e.block == event.block
                && matches!(
                    e.event,
                    Event::DepositMinted { .. }
                        | Event::RedeemRequested { .. }
                        | Event::RedeemRecredited { .. }
                ))
            .count()
            == 1,
        "cannot isolate restoration from other same-block supply changes"
    );
    let previous_number = event
        .block
        .number
        .checked_sub(1)
        .context("missing restoration predecessor")?;
    let previous = zama::watcher::Watcher::new(contract)
        .block(previous_number)
        .await?
        .reference;
    let included = zama::watcher::Watcher::new(contract)
        .block(event.block.number)
        .await?;
    ensure!(
        included.reference == event.block && included.parent_hash == previous.hash,
        "restoration block ancestry changed"
    );
    let record = contract.redemption_at(redeem.id, event.block).await?;
    ensure!(
        record.user == redeem.burner
            && record.amount == redeem.handle
            && record.status == RedemptionStatus::Recredited
            && record.zcash_txid.is_zero(),
        "restoration differs from original burn"
    );
    ensure!(
        contract.auditor().await? == auditor.address(),
        "supply auditor identity mismatch"
    );
    let before_handle = contract.supply_at(previous).await?;
    let after_handle = contract.supply_at(event.block).await?;
    let before = decrypt_actual(contract, sidecar, before_handle, auditor).await?;
    let after = decrypt_actual(contract, sidecar, after_handle, auditor).await?;
    contract.check_canonical(previous).await?;
    contract.check_canonical(event.block).await?;
    ensure!(
        after.checked_sub(before) == Some(redeem.actual),
        "restored supply differs from actual burn"
    );
    Ok(redeem.actual)
}

#[cfg(test)]
#[path = "bridge_redeem_tests.rs"]
mod tests;
