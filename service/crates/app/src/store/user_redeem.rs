//! User burns are bound to user-signed calldata; the operator never signs them.
use super::{Store, bridge::lock_bridge, fixed};
use alloy::{
    primitives::{Address, B256, Bytes, keccak256},
    sol_types::SolCall,
};
use anyhow::{Result, ensure};
use zama::contract::SignedTransaction;

pub struct UserRedeemRequest {
    pub id: String,
    pub user: Address,
    pub amount: u64,
    pub destination_hash: B256,
    pub calldata: Bytes,
    pub signed: Option<SignedTransaction>,
}
impl Store {
    /// Existing immutable preparations bind the destination key across restarts.
    /// Validate before scanning burns: a wrong key must not turn them into rejections.
    pub async fn check_destination_key(
        &self,
        key: &crate::core::destination::DestinationKey,
    ) -> Result<()> {
        let domain = self
            .http_domain()
            .await?
            .ok_or_else(|| anyhow::anyhow!("bridge domain missing"))?;
        for row in self
            .client
            .query(
                "SELECT user_address,destination_hash,calldata FROM bridge_user_requests",
                &[],
            )
            .await?
        {
            let user = Address::from(fixed::<20>(row.get("user_address"))?);
            let calldata: Vec<u8> = row.get("calldata");
            let call = zama::contract::bindings::requestRedeemCall::abi_decode(&calldata)?;
            let destination =
                key.decrypt_destination(domain.chain_id, domain.contract, user, &call.destCt)?;
            ensure!(
                keccak256(destination.as_bytes()).as_slice()
                    == row.get::<_, Vec<u8>>("destination_hash"),
                "destination key does not match saved preparation"
            );
        }
        // Also cover accepted burns that did not originate in the local helper.
        for row in self.client.query("SELECT burner,dest_ct,destination FROM bridge_redemptions WHERE destination IS NOT NULL", &[]).await? {
            let user = Address::from(fixed::<20>(row.get("burner"))?);
            let envelope: Vec<u8> = row.get("dest_ct");
            let destination = key.decrypt_destination(domain.chain_id, domain.contract, user, &envelope)?;
            ensure!(*destination == row.get::<_,String>("destination"), "destination key does not match saved burn");
        }
        Ok(())
    }

    pub async fn user_redeem_request(&self, id: &str) -> Result<Option<UserRedeemRequest>> {
        let row=self.client.query_opt("SELECT u.*,u.amount::text AS amount_text,d.chain_id,d.contract FROM bridge_user_requests u JOIN bridge_domain d ON true WHERE u.id=$1",&[&id]).await?;
        row.map(|r| {
            let user = Address::from(fixed::<20>(r.get("user_address"))?);
            let signed = r
                .get::<_, Option<Vec<u8>>>("signed_raw")
                .map(|raw| {
                    SignedTransaction::restore(
                        u64::try_from(r.get::<_, i64>("chain_id"))?,
                        Address::from(fixed::<20>(r.get("contract"))?),
                        user,
                        raw.into(),
                    )
                })
                .transpose()?;
            Ok(UserRedeemRequest {
                id: id.into(),
                user,
                amount: r.get::<_, String>("amount_text").parse()?,
                destination_hash: B256::from(fixed::<32>(r.get("destination_hash"))?),
                calldata: r.get::<_, Vec<u8>>("calldata").into(),
                signed,
            })
        })
        .transpose()
    }
    pub async fn save_user_redeem_request(
        &mut self,
        id: &str,
        user: Address,
        amount: u64,
        destination_hash: B256,
        calldata: &Bytes,
    ) -> Result<UserRedeemRequest> {
        ensure!(
            !id.is_empty() && id.len() <= 128 && !user.is_zero(),
            "invalid user request"
        );
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let n=tx.execute("INSERT INTO bridge_user_requests(id,user_address,amount,destination_hash,calldata) VALUES($1,$2,$3::text::numeric,$4,$5) ON CONFLICT(id) DO UPDATE SET id=EXCLUDED.id WHERE bridge_user_requests.user_address=EXCLUDED.user_address AND bridge_user_requests.amount=EXCLUDED.amount AND bridge_user_requests.destination_hash=EXCLUDED.destination_hash",&[&id,&user.as_slice(),&amount.to_string(),&destination_hash.as_slice(),&calldata.as_ref()]).await?;
        ensure!(n == 1, "user request identity already bound");
        tx.commit().await?;
        self.user_redeem_request(id)
            .await?
            .ok_or_else(|| anyhow::anyhow!("user request missing"))
    }
    pub async fn save_user_redeem_signed(
        &mut self,
        id: &str,
        signed: &SignedTransaction,
    ) -> Result<()> {
        let tx = self.client.transaction().await?;
        lock_bridge(&tx).await?;
        let r=tx.query_one("SELECT u.*,d.chain_id,d.contract FROM bridge_user_requests u JOIN bridge_domain d ON true WHERE u.id=$1 FOR UPDATE OF u",&[&id]).await?;
        ensure!(
            signed.sender().as_slice() == r.get::<_, Vec<u8>>("user_address")
                && signed.chain_id() == u64::try_from(r.get::<_, i64>("chain_id"))?
                && signed.contract().as_slice() == r.get::<_, Vec<u8>>("contract")
                && signed.matches_user_redeem(&r.get::<_, Vec<u8>>("calldata").into()),
            "user signature does not match prepared burn"
        );
        if let Some(old) = r.get::<_, Option<Vec<u8>>>("signed_raw") {
            ensure!(
                old == signed.raw().as_ref(),
                "user request already owns signed transaction"
            );
        } else {
            tx.execute(
                "UPDATE bridge_user_requests SET signed_raw=$2,tx_hash=$3 WHERE id=$1",
                &[&id, &signed.raw().as_ref(), &signed.hash().as_slice()],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}
