//! Read-only HTTP projections. Never authorize or advance bridge workflows.
use super::{Store, fixed};
use crate::core::bridge_types::BridgeDomain;
use alloy::primitives::Address;
use anyhow::Result;
use serde_json::{Value, json};

impl Store {
    pub async fn http_domain(&self) -> Result<Option<BridgeDomain>> {
        self.client
            .query_opt("SELECT chain_id,contract FROM bridge_domain", &[])
            .await?
            .map(|r| {
                Ok(BridgeDomain {
                    chain_id: u64::try_from(r.get::<_, i64>("chain_id"))?,
                    contract: Address::from(fixed::<20>(r.get("contract"))?),
                })
            })
            .transpose()
    }

    pub async fn http_health(&self) -> Result<Value> {
        let r = self.client.query_one("WITH report AS (SELECT b.zcash_height,b.zama_number FROM bridge_checkpoints b JOIN checkpoints z ON z.height=b.zcash_height AND z.hash=b.zcash_hash JOIN zama_blocks e ON e.number=b.zama_number AND e.hash=b.zama_hash WHERE b.id=(SELECT max(id) FROM bridge_checkpoints) AND b.valid AND b.coverage_complete AND b.fee_reserve>=10000) SELECT d.halted OR c.halted AS halted, a.current_height, c.tip_number, EXISTS(SELECT 1 FROM report) AS report_valid, EXISTS(SELECT 1 FROM report WHERE zcash_height=a.current_height-2 AND zama_number=c.tip_number-1) AS report_current FROM bridge_domain d JOIN zama_chain c ON true JOIN account a ON true", &[]).await?;
        Ok(
            json!({"halted": r.get::<_,bool>("halted"), "zcash_height":r.get::<_,i64>("current_height"), "evm_height":r.get::<_,i64>("tip_number"), "report_valid":r.get::<_,bool>("report_valid"), "report_current":r.get::<_,bool>("report_current")}),
        )
    }

    pub async fn http_payment_settled(&self, operation_id: &str) -> Result<bool> {
        Ok(self.client.query_one("SELECT EXISTS(SELECT 1 FROM bridge_redemptions WHERE operation_id=$1 AND canonical AND state IN ('paid','fulfilled')) OR EXISTS(SELECT 1 FROM bridge_refunds f JOIN bridge_deposits d USING(deposit_id) WHERE f.operation_id=$1 AND d.canonical AND f.state='paid')", &[&operation_id]).await?.get(0))
    }

    pub async fn http_reconcile(&self) -> Result<Option<Value>> {
        let row = self.client.query_opt("SELECT id,zcash_height,zcash_hash,zama_number,zama_hash,supply::text,assets::text,deposit_liability::text,unpaid_redemptions::text,fee_reserve::text,coverage_complete,valid FROM bridge_checkpoints ORDER BY id DESC LIMIT 1", &[]).await?;
        Ok(row.map(|r| json!({"checkpoint_id":r.get::<_,i64>("id").to_string(),"zcash_height":r.get::<_,i64>("zcash_height"),"evm_height":r.get::<_,i64>("zama_number"),"zcash_hash":hex::encode(r.get::<_,Vec<u8>>("zcash_hash")),"evm_hash":format!("0x{}",hex::encode(r.get::<_,Vec<u8>>("zama_hash"))),"supply_zat":r.get::<_,String>("supply"),"assets_zat":r.get::<_,String>("assets"),"deposit_liability_zat":r.get::<_,String>("deposit_liability"),"unpaid_redemptions_zat":r.get::<_,String>("unpaid_redemptions"),"fee_reserve_zat":r.get::<_,Option<String>>("fee_reserve"),"coverage_complete":r.get::<_,bool>("coverage_complete"),"valid":r.get::<_,bool>("valid")})))
    }
}
