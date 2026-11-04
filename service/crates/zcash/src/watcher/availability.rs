//! Scan-only handling for a tip advertised before compact blocks become available.
use std::time::Duration;

use anyhow::{Context, Result, ensure};

use super::Watcher;
use crate::{
    core::Checkpoint,
    rpc::{BlockOutOfRange, CompactBlock, TreeState},
};

impl Watcher {
    /// Return no partial batch and never change the selected scan target. A retry
    /// requires a GetBlock OUT_OF_RANGE within two blocks of both the selected
    /// and freshly validated tip, with the durable checkpoint still canonical.
    /// A stop returns None, allowing the caller to leave readiness false.
    pub async fn fetch_scan_batch_until(
        &self,
        checkpoint: &Checkpoint,
        end: u32,
        tip: u32,
        stopping: impl Fn() -> bool,
    ) -> Result<Option<Vec<(CompactBlock, TreeState)>>> {
        let start = checkpoint
            .height
            .checked_add(1)
            .context("scan height overflow")?;
        let delays = [1, 2, 4, 8];
        let mut retries = 0;
        loop {
            if stopping() {
                return Ok(None);
            }
            let error = match self.fetch_batch(start, end).await {
                Ok(batch) => return Ok(Some(batch)),
                Err(error) => error,
            };
            let Some(unavailable) = error.downcast_ref::<BlockOutOfRange>() else {
                return Err(error);
            };
            let height = unavailable.height;
            if height > tip || tip - height > 2 {
                return Err(error);
            }
            if retries == delays.len() {
                eprintln!(
                    "wallet scan: GetBlock availability exhausted height={height} target={tip} saved={} retries={retries} grpc_code=11",
                    checkpoint.height
                );
                return Err(error.context("near-tip GetBlock availability retries exhausted"));
            }
            let delay = delays[retries];
            retries += 1;
            eprintln!(
                "wallet scan: waiting for GetBlock height={height} target={tip} saved={} retry={retries} delay_secs={delay} grpc_code=11",
                checkpoint.height
            );
            // Poll only between RPCs and outside database writes. Avoid holding a
            // long sleep that would delay coordinator shutdown.
            for _ in 0..delay * 10 {
                if stopping() {
                    return Ok(None);
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            if stopping() {
                return Ok(None);
            }
            let refreshed_tip = self.client().tip().await.context("scan: retry tip")?;
            if stopping() {
                return Ok(None);
            }
            ensure!(
                refreshed_tip >= end && refreshed_tip >= height && refreshed_tip - height <= 2,
                "near-tip GetBlock retry tip changed"
            );
            ensure!(
                self.client()
                    .block_hash(checkpoint.height)
                    .await
                    .context("scan: retry checkpoint hash")?
                    == checkpoint.hash,
                "near-tip GetBlock retry checkpoint changed"
            );
        }
    }
}
