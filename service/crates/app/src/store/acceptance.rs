//! One durable testnet acceptance run; restarting never creates a second wallet action.
use super::{Store, bridge::lock_bridge, fixed};
use crate::core::{
    bridge_types::{BridgeCheckpointPair, BridgeDeposit, DepositState},
    redeem_types::{REDEEM_FEE_ZAT, RedeemState},
};
use alloy::primitives::{Address, B256, U256};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
pub const ACCEPTANCE_REQUEST: &str = "backed-roundtrip/user-redeem";
pub struct Acceptance {
    pub user: Address,
    pub destination: String,
    pub redeem_amount: u64,
    pub deposit: Option<BridgeDeposit>,
    pub balance_handle: Option<B256>,
}
impl Store {
    pub async fn start_bridge_acceptance(
        &mut self,
        user: Address,
        destination: &str,
        amount: u64,
    ) -> Result<()> {
        ensure!(
            !user.is_zero() && amount > REDEEM_FEE_ZAT,
            "invalid acceptance terms"
        );
        zcash::core::decode_address(destination)?;
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        ensure!(
            tx.query_opt("SELECT singleton FROM bridge_origin", &[])
                .await?
                .is_some(),
            "clean origin required"
        );
        let n=tx.execute("INSERT INTO bridge_acceptance VALUES(true,$1,$2,$3,NULL,NULL) ON CONFLICT(singleton) DO UPDATE SET singleton=true WHERE bridge_acceptance.user_address=EXCLUDED.user_address AND bridge_acceptance.destination=EXCLUDED.destination AND bridge_acceptance.redeem_amount=EXCLUDED.redeem_amount",&[&user.as_slice(),&destination,&i64::try_from(amount)?]).await?;
        ensure!(n == 1, "acceptance run already bound to different terms");
        tx.commit().await?;
        Ok(())
    }
    pub async fn bridge_acceptance(&mut self) -> Result<Acceptance> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let r = tx
            .query_one("SELECT * FROM bridge_acceptance FOR UPDATE", &[])
            .await
            .context("start acceptance with execution-time recipient/destination/amount")?;
        let user = Address::from(fixed::<20>(r.get("user_address"))?);
        let mut id = r.get::<_, Option<Vec<u8>>>("deposit_id");
        if id.is_none() {
            let rows = tx
                .query(
                    "SELECT deposit_id FROM bridge_deposits WHERE canonical AND recipient=$1",
                    &[&user.as_slice()],
                )
                .await?;
            ensure!(
                rows.len() <= 1,
                "multiple deposits; explicit review required, do not repeat deposit"
            );
            if let Some(d) = rows.first() {
                id = Some(d.get(0));
                tx.execute("UPDATE bridge_acceptance SET deposit_id=$1", &[&id])
                    .await?;
            }
        }
        let destination = r.get("destination");
        let amount = u64::try_from(r.get::<_, i64>("redeem_amount"))?;
        let balance_handle = r
            .get::<_, Option<Vec<u8>>>("balance_handle")
            .map(|b| fixed::<32>(b).map(B256::from))
            .transpose()?;
        tx.commit().await?;
        Ok(Acceptance {
            user,
            destination,
            redeem_amount: amount,
            balance_handle,
            deposit: match id {
                Some(id) => self.bridge_deposit(B256::from(fixed::<32>(id)?)).await?,
                None => None,
            },
        })
    }
    pub async fn record_acceptance_balance(&mut self, handle: B256, actual: u64) -> Result<()> {
        let a = self.bridge_acceptance().await?;
        let d = a.deposit.context("deposit missing")?;
        ensure!(
            d.state == DepositState::Minted
                && d.recipient == Some(a.user)
                && actual == d.amount
                && !handle.is_zero(),
            "private balance differs from exact deposit issuance"
        );
        ensure!(
            self.user_redeem_request(ACCEPTANCE_REQUEST)
                .await?
                .is_none(),
            "verify initial balance before redeem preparation"
        );
        self.client
            .execute(
                "UPDATE bridge_acceptance SET balance_handle=$1",
                &[&handle.as_slice()],
            )
            .await?;
        Ok(())
    }
    pub async fn acceptance_evidence(&mut self) -> Result<Value> {
        let a = self.bridge_acceptance().await?;
        let d = a.deposit.context("waiting for original memo deposit")?;
        ensure!(
            d.state == DepositState::Minted && a.balance_handle.is_some(),
            "exact mint and wallet balance verification required"
        );
        let request = self
            .user_redeem_request(ACCEPTANCE_REQUEST)
            .await?
            .context("user redemption not prepared")?;
        let signed = request
            .signed
            .context("actual user's signed burn required")?;
        let rs = self.bridge_redeems().await?;
        let r = rs
            .iter()
            .find(|r| r.source_tx == signed.hash())
            .context("burn not confirmed")?;
        ensure!(
            r.canonical
                && r.state == RedeemState::Fulfilled
                && r.burner == a.user
                && r.actual == a.redeem_amount
                && r.destination.as_deref() == Some(&a.destination),
            "round trip not fulfilled with exact user terms"
        );
        let p = self
            .get_attempt(&r.operation_id)
            .await?
            .context("payment journal missing")?
            .prepared
            .context("signed payment missing")?;
        let pair = self.bridge_checkpoint_pair().await?;
        let check=self.client.query_opt("SELECT id FROM bridge_checkpoints WHERE valid AND coverage_complete AND fee_reserve>=0 AND zcash_height=$1 AND zcash_hash=$2 AND zama_number=$3 AND zama_hash=$4 ORDER BY id DESC LIMIT 1",&[&i64::from(pair.zcash_height),&&pair.zcash_hash[..],&i64::try_from(pair.checkpoint.number)?,&pair.checkpoint.hash.as_slice()]).await?.context("current reconciled accounting required")?;
        let fulfillment = self
            .zama_transaction(&r.fulfillment_id())
            .await?
            .context("fulfillment journal missing")?;
        ensure!(
            fulfillment.confirmed && fulfillment.state == super::TransactionState::Successful,
            "fulfillment needs two canonical confirmations"
        );
        let mint = self
            .zama_transaction(&d.operation_id)
            .await?
            .context("automatic mint journal missing")?;
        ensure!(
            mint.confirmed && mint.state == super::TransactionState::Successful,
            "mint needs two confirmations"
        );
        let origin = self.bridge_origin().await?.context("origin missing")?;
        let vault = self.load().await?;
        Ok(
            json!({"chain_id":11155111,"contract":mint.signed.contract(),"deployment_transaction":origin.transaction,"deployment_block":origin.block,"vault_identity":alloy::primitives::keccak256(vault.account.fvk),"vault_birthday":vault.account.birthday,"deposit_id":d.deposit_id,"deposit_transaction":crate::api::txid_text(d.note_id.txid),"mint_transaction":mint.signed.hash(),"redeem_transaction":signed.hash(),"redeem_id":r.id.to_string(),"payout_transaction":crate::api::txid_text(p.txid),"fulfillment_transaction":fulfillment.signed.hash(),"accounting_checkpoint":check.get::<_,i64>(0),"zcash_checkpoint":pair.zcash_height,"evm_checkpoint":pair.checkpoint,"checks":{"initial_supply_verified_zero":true,"exact_issuance":true,"user_authorized_balance_matches":true,"user_authorized_burn":true,"exact_burn":true,"destination_matches":true,"confirmed_spend_and_change":true,"fulfillment_confirmed":true,"accounting_complete_and_solvent":true},"limits":["two EVM confirmations remain reorgable","controlled reorg and expiry tests are separate from live evidence","refunds and re-credit remain disabled"]}),
        )
    }
}

impl Store {
    /// Sanitized acceptance only. The coordinator has already privately checked
    /// restoration and confirmed spend/change; revalidate durable canonical facts.
    pub async fn recovery_evidence(
        &mut self,
        redeem_id: U256,
        deposit_id: B256,
        checkpoint_id: Option<i64>,
    ) -> Result<(BridgeCheckpointPair, Value)> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let checkpoint = tx.query_opt("SELECT c.id,c.zcash_height,c.zcash_hash,c.zama_number,c.zama_hash FROM bridge_checkpoints c JOIN checkpoints z ON z.height=c.zcash_height AND z.hash=c.zcash_hash JOIN zama_blocks e ON e.number=c.zama_number AND e.hash=c.zama_hash CROSS JOIN account a CROSS JOIN zama_chain s WHERE c.valid AND c.coverage_complete AND c.fee_reserve>=0 AND c.assets=c.supply+c.deposit_liability+c.unpaid_redemptions+c.fee_reserve AND c.zcash_height<=a.current_height-2 AND c.zama_number<s.tip_number AND ($1::bigint IS NULL OR c.id=$1) ORDER BY c.zama_number DESC,c.zcash_height DESC,c.id DESC LIMIT 1", &[&checkpoint_id]).await?.context("completed canonical recovery accounting unavailable")?;
        let accounting_id: i64 = checkpoint.get("id");
        let pair = BridgeCheckpointPair {
            zcash_height: u32::try_from(checkpoint.get::<_, i64>("zcash_height"))?,
            zcash_hash: fixed(checkpoint.get("zcash_hash"))?,
            checkpoint: zama::watcher::BlockRef {
                number: u64::try_from(checkpoint.get::<_, i64>("zama_number"))?,
                hash: B256::from(fixed::<32>(checkpoint.get("zama_hash"))?),
            },
        };
        let row = tx.query_one("SELECT r.actual::text,r.burner,r.restored_handle,r.recredited_number,r.recredited_hash,t.tx_hash FROM bridge_redemptions r JOIN zama_transactions t ON t.operation_id='cwzec/recredit/'||r.redeem_id JOIN zama_receipts e ON e.tx_hash=t.tx_hash WHERE r.redeem_id=$1 AND r.state='recredited' AND r.canonical AND r.actual>0 AND r.recredited_number<=$2 AND e.canonical AND e.confirmed AND e.success AND e.acted_on AND e.block_hash=r.recredited_hash AND NOT EXISTS(SELECT 1 FROM operations o WHERE o.id=r.operation_id)", &[&redeem_id.to_string(), &i64::try_from(pair.checkpoint.number)?]).await?;
        ensure!(
            row.get::<_, Option<Vec<u8>>>("restored_handle").is_some(),
            "restoration evidence missing"
        );
        let refund = tx.query_one("SELECT f.operation_id,f.void_operation_id,f.paid_height,f.paid_hash,o.txid,o.signed_bytes,o.expiry_height,o.fee,o.amount,f.gross_amount FROM bridge_refunds f JOIN bridge_deposits d USING(deposit_id) JOIN operations o ON o.id=f.operation_id JOIN checkpoints c ON c.height=f.paid_height AND c.hash=f.paid_hash WHERE f.deposit_id=$1 AND f.state='paid' AND d.canonical AND d.state='voided' AND d.acted_on AND NOT d.claimed AND d.observed_number<=$2 AND f.paid_height<=$3 AND o.destination=f.destination AND o.amount+o.fee=f.gross_amount AND f.gross_amount=d.amount AND o.fee=f.fee AND o.signed_bytes IS NOT NULL", &[&deposit_id.as_slice(), &i64::try_from(pair.checkpoint.number)?, &i64::from(pair.zcash_height)]).await?;
        let void = tx.query_one("SELECT t.tx_hash FROM zama_transactions t JOIN zama_receipts e ON e.tx_hash=t.tx_hash WHERE t.operation_id=$1 AND e.canonical AND e.confirmed AND e.success AND e.acted_on", &[&refund.get::<_,String>("void_operation_id")]).await?;
        let result = json!({"redeem_id":redeem_id.to_string(),"deposit_id":deposit_id,"recredit_transaction":B256::from(fixed::<32>(row.get("tx_hash"))?),"void_transaction":B256::from(fixed::<32>(void.get("tx_hash"))?),"refund_transaction":crate::api::txid_text(fixed::<32>(refund.get("txid"))?),"accounting_checkpoint":accounting_id,"zcash_checkpoint":pair.zcash_height,"zcash_checkpoint_hash":B256::from(pair.zcash_hash),"evm_checkpoint":pair.checkpoint,"checks":{"original_burner_actual_restoration":true,"no_redemption_payment_issued":true,"known_participant_terms_immutable":true,"void_confirmed_before_refund":true,"refund_spend_change_and_fee_confirmed":true,"accounting_conserved":true},"limits":["issued-attempt death recovery remains disabled: historical absence is not established","two EVM confirmations remain reorgable","death and reorg fault tests are controlled, not live evidence"]});
        if checkpoint_id.is_some() {
            tx.execute(
                "UPDATE bridge_checkpoints SET acted_on=true WHERE id=$1",
                &[&accounting_id],
            )
            .await?;
        }
        tx.commit().await?;
        Ok((pair, result))
    }
}
