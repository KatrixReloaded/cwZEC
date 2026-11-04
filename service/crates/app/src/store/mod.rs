//! PostgreSQL persistence for the single testnet Ironwood account.
//! Reservation rows and issued bytes are permanent; only confirmed proven-death
//! restoration releases reservation ownership. Chain rewinds never release it.
mod acceptance;
#[cfg(test)]
mod acceptance_tests;
mod bridge;
mod http;
mod payout_death;
#[cfg(test)]
mod payout_death_test_support;
#[cfg(test)]
mod payout_death_tests;
pub use payout_death::DeathCollection;
#[cfg(test)]
mod http_tests;
mod redeem;
mod refund;
mod user_redeem;
pub use acceptance::{ACCEPTANCE_REQUEST, Acceptance};
pub use user_redeem::UserRedeemRequest;
#[cfg(test)]
pub(crate) mod bridge_tests;
#[cfg(test)]
mod recovery_tests;
#[cfg(test)]
mod recredit_tests;
#[cfg(test)]
mod roundtrip_tests;
#[cfg(test)]
mod user_redeem_tests;
mod zama;
pub use zama::{TransactionState, ZamaCursor, ZamaTransaction};
#[cfg(test)]
pub(crate) mod zama_test_support;
#[cfg(test)]
mod zama_tests;
use anyhow::{Context, Result, ensure};
use tokio_postgres::{Client, NoTls, Row, Transaction};
use zcash::core::{
    Account, Checkpoint, NoteId, PreparedTransaction, ScanUpdate, WalletNote, WalletState,
};

pub struct Store {
    client: Client,
}

pub struct Attempt {
    pub operation_id: String,
    pub destination: String,
    pub amount: u64,
    pub inputs: Vec<NoteId>,
    pub prepared: Option<PreparedTransaction>,
    pub broadcast: bool,
    pub observed_height: Option<u32>,
}

pub struct DepositRecord {
    pub id: NoteId,
    pub value: u64,
    pub rho: [u8; 32],
    pub rseed: [u8; 32],
    pub memo: [u8; 512],
    pub internal: bool,
    pub mined_height: u32,
    pub spent_height: Option<u32>,
}

impl Store {
    /// The connection owns this process-wide coordinator lease until it closes.
    /// Separate HTTP connections still use the normal transactional admission locks.
    pub async fn claim_coordinator(&self) -> Result<()> {
        let acquired: bool = self
            .client
            .query_one("SELECT pg_try_advisory_lock(1668774501, 1)", &[])
            .await?
            .get(0);
        ensure!(acquired, "bridge coordinator already running");
        Ok(())
    }

    pub fn check_coordinator_connection(&self) -> Result<()> {
        ensure!(
            !self.client.is_closed(),
            "coordinator lease connection closed"
        );
        Ok(())
    }

    pub async fn checkpoint(&self, height: u32) -> Result<Option<Checkpoint>> {
        self.client
            .query_opt(
                "SELECT * FROM checkpoints WHERE height=$1",
                &[&(height as i64)],
            )
            .await?
            .map(|row| {
                Ok(Checkpoint {
                    height,
                    hash: fixed(row.get("hash"))?,
                    parent: fixed(row.get("parent"))?,
                    frontier: row.get("frontier"),
                })
            })
            .transpose()
    }

    pub async fn deposits(&self) -> Result<Vec<DepositRecord>> {
        self.client.query("SELECT txid,action_index,value,rho,rseed,memo,internal,mined_height,spent_height FROM notes WHERE canonical ORDER BY mined_height,position", &[]).await?
            .into_iter().map(|r| Ok(DepositRecord { id: NoteId { txid: fixed(r.get("txid"))?, action_index: r.get::<_,i64>("action_index") as u32 }, value: r.get::<_,i64>("value") as u64, rho: fixed(r.get("rho"))?, rseed: fixed(r.get("rseed"))?, memo: fixed(r.get("memo"))?, internal: r.get("internal"), mined_height: r.get::<_,i64>("mined_height") as u32, spent_height: r.get::<_,Option<i64>>("spent_height").map(|h|h as u32) })).collect()
    }

    pub async fn connect(url: &str) -> Result<Self> {
        let (client, connection) = tokio_postgres::connect(url, NoTls).await?;
        tokio::spawn(async move {
            let _ = connection.await;
        });
        Ok(Self { client })
    }

    pub async fn is_initialized(&self) -> Result<bool> {
        Ok(self
            .client
            .query_one("SELECT EXISTS(SELECT 1 FROM account)", &[])
            .await?
            .get(0))
    }

    pub async fn bridge_enabled(&self) -> Result<bool> {
        Ok(self
            .client
            .query_one("SELECT EXISTS(SELECT 1 FROM bridge_domain)", &[])
            .await?
            .get(0))
    }

    /// Active ownership, including orphaned and spent inputs. Released historical
    /// rows remain available through their original attempt.
    pub async fn reserved_inputs(&self) -> Result<Vec<(NoteId, [u8; 32])>> {
        self.client
            .query(
                "SELECT txid,action_index,nullifier FROM reservations WHERE NOT released ORDER BY txid,action_index",
                &[],
            )
            .await?
            .into_iter()
            .map(|r| {
                Ok((
                    NoteId {
                        txid: fixed(r.get(0))?,
                        action_index: r.get::<_, i64>(1) as u32,
                    },
                    fixed(r.get(2))?,
                ))
            })
            .collect()
    }

    pub async fn migrate(&mut self) -> Result<()> {
        let tx = self.client.transaction().await?;
        tx.batch_execute(include_str!("../../migrations/001_wallet.sql"))
            .await?;
        tx.batch_execute(include_str!("../../migrations/002_zama.sql"))
            .await?;
        tx.batch_execute(include_str!("../../migrations/003_zama_confirmations.sql"))
            .await?;
        tx.batch_execute(include_str!("../../migrations/004_bridge.sql"))
            .await?;
        tx.batch_execute(include_str!("../../migrations/005_roundtrip.sql"))
            .await?;
        tx.batch_execute(include_str!("../../migrations/006_recovery.sql"))
            .await?;
        tx.batch_execute(include_str!("../../migrations/007_payout_death.sql"))
            .await?;
        tx.commit().await?;
        Ok(())
    }

    /// First checkpoint is the chain state immediately before scanning starts.
    pub async fn initialize(&mut self, account: &Account, checkpoint: &Checkpoint) -> Result<()> {
        let tx = self.client.transaction().await?;
        tx.batch_execute("LOCK TABLE account IN EXCLUSIVE MODE")
            .await?;
        if let Some(row) = tx
            .query_opt("SELECT birthday,fvk,use_qsk FROM account", &[])
            .await?
        {
            ensure!(
                row.get::<_, i64>(0) == account.birthday as i64
                    && row.get::<_, Vec<u8>>(1) == account.fvk
                    && row.get::<_, bool>(2) == account.use_qsk,
                "account identity mismatch"
            );
        } else {
            ensure!(
                u64::from(account.birthday) == u64::from(checkpoint.height) + 1,
                "birthday must follow initial checkpoint"
            );
            insert_checkpoint(&tx, checkpoint).await?;
            tx.execute("INSERT INTO account(singleton,network,birthday,fvk,use_qsk,current_height) VALUES(true,'testnet',$1,$2,$3,$4)", &[&(account.birthday as i64), &&account.fvk[..], &account.use_qsk, &(checkpoint.height as i64)]).await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn load(&mut self) -> Result<WalletState> {
        let tx = self.client.transaction().await?;
        let row = lock_account(&tx).await?;
        let height: i64 = row.get("current_height");
        let account = Account {
            birthday: row.get::<_, i64>("birthday") as u32,
            fvk: fixed(row.get("fvk"))?,
            use_qsk: row.get("use_qsk"),
        };
        let row = tx
            .query_one("SELECT * FROM checkpoints WHERE height=$1", &[&height])
            .await?;
        let checkpoint = Checkpoint {
            height: height as u32,
            hash: fixed(row.get("hash"))?,
            parent: fixed(row.get("parent"))?,
            frontier: row.get("frontier"),
        };
        let rows = tx
            .query(
                "SELECT n.*,w.bytes AS witness
                FROM notes n
                JOIN witnesses w USING(txid,action_index)
                WHERE n.canonical
                AND n.spent_height IS NULL
                AND w.height=$1
                ORDER BY n.position",
                &[&height],
            )
            .await?;
        let notes = rows
            .into_iter()
            .map(note_from_row)
            .collect::<Result<Vec<_>>>()?;
        tx.commit().await?;
        Ok(WalletState {
            account,
            checkpoint,
            notes,
        })
    }

    pub async fn commit_scan(&mut self, update: &ScanUpdate) -> Result<()> {
        let tx = self.client.transaction().await?;
        let account = lock_account(&tx).await?;
        let height: i64 = account.get("current_height");
        ensure!(
            u64::from(update.checkpoint.height) == height as u64 + 1,
            "nonconsecutive checkpoint"
        );
        let old = tx
            .query_one("SELECT hash FROM checkpoints WHERE height=$1", &[&height])
            .await?;
        ensure!(
            old.get::<_, Vec<u8>>(0) == update.checkpoint.parent,
            "checkpoint parent mismatch"
        );
        insert_checkpoint(&tx, &update.checkpoint).await?;
        for n in &update.received {
            ensure!(
                n.mined_height == update.checkpoint.height && n.spent_height.is_none(),
                "invalid received note height or spend"
            );
            let position = i64::try_from(n.position)?;
            let value = i64::try_from(n.value)?;
            let count = tx
                .execute(
                    "INSERT INTO notes(
                    txid,action_index,recipient,value,rho,rseed,nullifier,
                    position,internal,memo,mined_height
                ) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)
                ON CONFLICT(txid,action_index) DO UPDATE SET
                    canonical=true,mined_height=EXCLUDED.mined_height,
                    position=EXCLUDED.position,spent_height=NULL
                WHERE NOT notes.canonical
                    AND notes.recipient=EXCLUDED.recipient
                    AND notes.value=EXCLUDED.value
                    AND notes.rho=EXCLUDED.rho
                    AND notes.rseed=EXCLUDED.rseed
                    AND notes.nullifier=EXCLUDED.nullifier
                    AND notes.internal=EXCLUDED.internal
                    AND notes.memo=EXCLUDED.memo",
                    &[
                        &&n.id.txid[..],
                        &(n.id.action_index as i64),
                        &&n.recipient[..],
                        &value,
                        &&n.rho[..],
                        &&n.rseed[..],
                        &&n.nullifier[..],
                        &position,
                        &n.internal,
                        &&n.memo[..],
                        &(n.mined_height as i64),
                    ],
                )
                .await?;
            ensure!(count == 1, "conflicting received note");
        }
        for nf in &update.spent_nullifiers {
            tx.execute("UPDATE notes SET spent_height=$1 WHERE canonical AND nullifier=$2 AND spent_height IS NULL", &[&(update.checkpoint.height as i64), &&nf[..]]).await?;
        }
        for n in &update.received {
            // Preserve the initial witness supplied with each received note.
            insert_witness(&tx, update.checkpoint.height, &n.id, &n.witness).await?;
        }
        for (id, witness) in &update.witnesses {
            insert_witness(&tx, update.checkpoint.height, id, witness).await?;
        }
        let missing: i64 = tx
            .query_one(
                "SELECT count(*)
                FROM notes n
                WHERE canonical
                AND spent_height IS NULL
                AND NOT EXISTS(SELECT 1
                FROM witnesses w
                WHERE w.height=$1
                AND w.txid=n.txid
                AND w.action_index=n.action_index)",
                &[&(update.checkpoint.height as i64)],
            )
            .await?
            .get(0);
        ensure!(missing == 0, "missing checkpoint witness");
        for id in &update.txids {
            tx.execute("INSERT INTO observed_transactions(txid,height) VALUES($1,$2) ON CONFLICT DO NOTHING", &[&&id[..], &(update.checkpoint.height as i64)]).await?;
        }
        tx.execute(
            "UPDATE account SET current_height=$1",
            &[&(update.checkpoint.height as i64)],
        )
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Rewind only to an explicitly retained checkpoint with the expected hash.
    pub async fn rewind(&mut self, height: u32, hash: [u8; 32]) -> Result<()> {
        let tx = self.client.transaction().await?;
        let row = lock_account(&tx).await?;
        ensure!(
            height as i64 <= row.get::<_, i64>("current_height"),
            "rewind ahead of tip"
        );
        let row = tx
            .query_opt(
                "SELECT hash FROM checkpoints WHERE height=$1",
                &[&(height as i64)],
            )
            .await?
            .context("checkpoint not retained")?;
        ensure!(row.get::<_, Vec<u8>>(0) == hash, "rewind hash mismatch");
        if bridge::bridge_zcash_rewind(&tx, height).await? {
            tx.commit().await?;
            anyhow::bail!("Zcash reorg affects bridge action; reconciliation halted");
        }
        tx.execute(
            "UPDATE notes SET canonical=false WHERE mined_height>$1",
            &[&(height as i64)],
        )
        .await?;
        tx.execute(
            "UPDATE notes SET spent_height=NULL WHERE spent_height>$1",
            &[&(height as i64)],
        )
        .await?;
        tx.execute(
            "DELETE FROM checkpoints WHERE height>$1",
            &[&(height as i64)],
        )
        .await?;
        tx.execute("UPDATE account SET current_height=$1", &[&(height as i64)])
            .await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn reserve(
        &mut self,
        operation_id: &str,
        destination: &str,
        amount: u64,
        inputs: &[NoteId],
        min_confirmations: u32,
    ) -> Result<()> {
        ensure!(
            !inputs.is_empty() && min_confirmations > 0,
            "inputs and confirmations required"
        );
        unique_inputs(inputs)?;
        let tx = self.client.transaction().await?;
        let account = lock_account(&tx).await?;
        ensure!(
            !tx.query_one("SELECT EXISTS(SELECT 1 FROM bridge_domain)", &[])
                .await?
                .get::<_, bool>(0),
            "legacy operator payout disabled in bridge domain"
        );
        if let Some(existing) = read_attempt(&tx, operation_id).await? {
            ensure!(
                existing.destination == destination
                    && existing.amount == amount
                    && same_inputs(&existing.inputs, inputs),
                "operation terms mismatch"
            );
        } else {
            let amount = i64::try_from(amount)?;
            tx.execute(
                "INSERT INTO operations(id,destination,amount) VALUES($1,$2,$3)",
                &[&operation_id, &destination, &amount],
            )
            .await?;
            let tip: i64 = account.get("current_height");
            for id in inputs {
                let row = tx.query_opt("SELECT mined_height,spent_height,canonical,nullifier FROM notes WHERE txid=$1 AND action_index=$2", &[&&id.txid[..], &(id.action_index as i64)]).await?.context("unknown input")?;
                ensure!(
                    row.get::<_, bool>("canonical")
                        && row.get::<_, Option<i64>>("spent_height").is_none()
                        && tip - row.get::<_, i64>("mined_height") + 1 >= min_confirmations as i64,
                    "input unavailable or unconfirmed"
                );
                tx.execute(
                    "INSERT INTO reservations(txid,action_index,operation_id,nullifier) VALUES($1,$2,$3,$4)",
                    &[&&id.txid[..], &(id.action_index as i64), &operation_id, &row.get::<_,Vec<u8>>("nullifier")],
                )
                .await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn save_prepared(
        &mut self,
        operation_id: &str,
        prepared: &PreparedTransaction,
    ) -> Result<()> {
        ensure!(
            prepared.expiry_height > 0 && !prepared.bytes.is_empty(),
            "signed bytes and nonzero expiry required"
        );
        unique_inputs(&prepared.inputs)?;
        let tx = self.client.transaction().await?;
        lock_account(&tx).await?;
        refund::validate_prepared(&tx, operation_id).await?;
        if let Some(r) = tx
            .query_opt(
                "SELECT source_tx,state FROM bridge_redemptions WHERE operation_id=$1",
                &[&operation_id],
            )
            .await?
        {
            ensure!(
                prepared.fee == zcash::vault::fee_for_inputs(prepared.inputs.len())?,
                "payout network fee mismatch"
            );
            bridge::lock_bridge(&tx).await?;
            ensure!(
                r.get::<_, String>("state") == "reserved",
                "redemption is not reserved"
            );
            let valid: bool = tx.query_one("SELECT EXISTS(SELECT 1 FROM zama_receipts WHERE tx_hash=$1 AND canonical AND confirmed AND success AND acted_on)", &[&r.get::<_,Vec<u8>>("source_tx")]).await?.get(0);
            ensure!(valid, "burn authorization no longer canonical");
        }
        let attempt = read_attempt(&tx, operation_id)
            .await?
            .context("operation not reserved")?;
        ensure!(
            same_inputs(&attempt.inputs, &prepared.inputs),
            "prepared inputs mismatch"
        );
        if let Some(old) = attempt.prepared {
            ensure!(
                old.bytes == prepared.bytes
                    && old.txid == prepared.txid
                    && old.expiry_height == prepared.expiry_height
                    && old.fee == prepared.fee,
                "issued transaction is immutable"
            );
        } else {
            let backed: bool = tx
                .query_one("SELECT EXISTS(SELECT 1 FROM bridge_domain)", &[])
                .await?
                .get(0);
            if backed {
                bridge::lock_bridge(&tx).await?;
                bridge::check_payment_accounting(&tx).await?;
                tx.execute("UPDATE bridge_checkpoints SET acted_on=true WHERE id=(SELECT max(id) FROM bridge_checkpoints)", &[]).await?;
            }
            // A rewind or concurrent spend may have invalidated a reservation before signing.
            for id in &prepared.inputs {
                let available: bool = tx.query_one("SELECT canonical AND spent_height IS NULL FROM notes WHERE txid=$1 AND action_index=$2", &[&&id.txid[..], &(id.action_index as i64)]).await?.get(0);
                ensure!(available, "reserved input no longer available");
            }
            tx.execute(
                "UPDATE operations SET signed_bytes=$2,txid=$3,expiry_height=$4,fee=$5 WHERE id=$1",
                &[
                    &operation_id,
                    &prepared.bytes,
                    &&prepared.txid[..],
                    &(prepared.expiry_height as i64),
                    &i64::try_from(prepared.fee)?,
                ],
            )
            .await?;
        }
        tx.commit().await?;
        Ok(())
    }

    pub async fn mark_broadcast(&mut self, operation_id: &str) -> Result<()> {
        ensure!(
            self.client
                .execute(
                    "UPDATE operations SET broadcast=true WHERE id=$1 AND signed_bytes IS NOT NULL AND NOT EXISTS(SELECT 1 FROM bridge_payout_death WHERE operation_id=$1 AND acted_on)",
                    &[&operation_id]
                )
                .await?
                == 1,
            "prepared operation required"
        );
        Ok(())
    }

    pub async fn get_attempt(&mut self, operation_id: &str) -> Result<Option<Attempt>> {
        let tx = self.client.transaction().await?;
        lock_account(&tx).await?;
        let attempt = read_attempt(&tx, operation_id).await?;
        tx.commit().await?;
        Ok(attempt)
    }

    /// Every issued journal entry, including paid entries. Never infer issuance
    /// from the broadcast acknowledgement; signed bytes are the boundary.
    pub async fn issued_attempts(&mut self) -> Result<Vec<Attempt>> {
        let tx = self.client.transaction().await?;
        let mut attempts = Vec::new();
        for row in tx
            .query(
                "SELECT id FROM operations WHERE signed_bytes IS NOT NULL ORDER BY id",
                &[],
            )
            .await?
        {
            attempts.push(
                read_attempt(&tx, row.get(0))
                    .await?
                    .context("issued attempt disappeared")?,
            );
        }
        tx.commit().await?;
        Ok(attempts)
    }

    /// Includes reserved but unprepared operations and issued transactions orphaned by rewind.
    pub async fn unfinished(&mut self) -> Result<Vec<Attempt>> {
        let tx = self.client.transaction().await?;
        lock_account(&tx).await?;
        let rows = tx.query("SELECT id FROM operations o WHERE NOT EXISTS(SELECT 1 FROM observed_transactions t WHERE t.txid=o.txid) AND NOT EXISTS(SELECT 1 FROM bridge_payout_death d WHERE d.operation_id=o.id AND d.acted_on) ORDER BY id", &[]).await?;
        let mut attempts = Vec::new();
        for row in rows {
            attempts.push(
                read_attempt(&tx, &row.get::<_, String>(0))
                    .await?
                    .context("missing operation")?,
            );
        }
        tx.commit().await?;
        Ok(attempts)
    }
}

fn fixed<const N: usize>(bytes: Vec<u8>) -> Result<[u8; N]> {
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("invalid stored field length"))
}
fn same_inputs(a: &[NoteId], b: &[NoteId]) -> bool {
    a.len() == b.len() && a.iter().all(|n| b.contains(n))
}
fn unique_inputs(inputs: &[NoteId]) -> Result<()> {
    for (i, id) in inputs.iter().enumerate() {
        ensure!(!inputs[..i].contains(id), "duplicate input");
    }
    Ok(())
}
async fn lock_account(tx: &Transaction<'_>) -> Result<Row> {
    Ok(tx
        .query_opt("SELECT * FROM account FOR UPDATE", &[])
        .await?
        .context("wallet not initialized")?)
}
async fn insert_checkpoint(tx: &Transaction<'_>, c: &Checkpoint) -> Result<()> {
    tx.execute(
        "INSERT INTO checkpoints(height,hash,parent,frontier) VALUES($1,$2,$3,$4)",
        &[
            &(c.height as i64),
            &&c.hash[..],
            &&c.parent[..],
            &c.frontier,
        ],
    )
    .await?;
    Ok(())
}
async fn insert_witness(
    tx: &Transaction<'_>,
    height: u32,
    id: &NoteId,
    bytes: &[u8],
) -> Result<()> {
    ensure!(!bytes.is_empty(), "empty witness");
    ensure!(
        tx.execute(
            "INSERT INTO witnesses(height,txid,action_index,bytes)
                SELECT $1,txid,action_index,$4
                FROM notes
                WHERE txid=$2
                AND action_index=$3
                AND canonical ON CONFLICT(height,txid,action_index)
                DO UPDATE SET bytes=EXCLUDED.bytes
                WHERE witnesses.bytes=EXCLUDED.bytes",
            &[
                &(height as i64),
                &&id.txid[..],
                &(id.action_index as i64),
                &bytes
            ]
        )
        .await?
            == 1,
        "unknown note or conflicting witness"
    );
    Ok(())
}
fn note_from_row(row: Row) -> Result<WalletNote> {
    Ok(WalletNote {
        id: NoteId {
            txid: fixed(row.get("txid"))?,
            action_index: row.get::<_, i64>("action_index") as u32,
        },
        recipient: fixed(row.get("recipient"))?,
        value: row.get::<_, i64>("value") as u64,
        rho: fixed(row.get("rho"))?,
        rseed: fixed(row.get("rseed"))?,
        nullifier: fixed(row.get("nullifier"))?,
        position: row.get::<_, i64>("position") as u64,
        internal: row.get("internal"),
        memo: fixed(row.get("memo"))?,
        mined_height: row.get::<_, i64>("mined_height") as u32,
        spent_height: row.get::<_, Option<i64>>("spent_height").map(|h| h as u32),
        witness: row.get("witness"),
    })
}
async fn read_attempt(tx: &Transaction<'_>, id: &str) -> Result<Option<Attempt>> {
    let Some(row) = tx.query_opt("SELECT o.*,(SELECT min(height) FROM observed_transactions t WHERE t.txid=o.txid) AS observed_height FROM operations o WHERE id=$1", &[&id]).await? else { return Ok(None); };
    let inputs = tx.query("SELECT txid,action_index FROM reservations WHERE operation_id=$1 ORDER BY txid,action_index", &[&id]).await?.into_iter().map(|r| Ok(NoteId { txid: fixed(r.get(0))?, action_index: r.get::<_,i64>(1) as u32 })).collect::<Result<Vec<_>>>()?;
    let prepared = row
        .get::<_, Option<Vec<u8>>>("signed_bytes")
        .map(|bytes| -> Result<_> {
            Ok(PreparedTransaction {
                bytes,
                txid: fixed(row.get("txid"))?,
                inputs: inputs.clone(),
                expiry_height: row.get::<_, i64>("expiry_height") as u32,
                fee: row.get::<_, i64>("fee") as u64,
            })
        })
        .transpose()?;
    Ok(Some(Attempt {
        operation_id: id.to_owned(),
        destination: row.get("destination"),
        amount: row.get::<_, i64>("amount") as u64,
        inputs,
        prepared,
        broadcast: row.get("broadcast"),
        observed_height: row
            .get::<_, Option<i64>>("observed_height")
            .map(|h| h as u32),
    }))
}
