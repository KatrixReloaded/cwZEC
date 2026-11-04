//! Public RPC diagnostics only: no wallet, database, signing or broadcast.
use anyhow::{Result, ensure};
use zcash::rpc::LightwalletClient;

#[tokio::main]
async fn main() -> Result<()> {
    let height = std::env::args()
        .nth(1)
        .map(|value| value.parse::<u32>())
        .transpose()?
        .unwrap_or(4_564_502);
    ensure!(height > 0 && height < u32::MAX, "height out of range");
    let endpoint = std::env::var("ZCASH_LIGHTWALLETD_URL")
        .unwrap_or_else(|_| "https://testnet.zec.rocks:443".into());
    let client = LightwalletClient::connect(&endpoint).await?;
    println!("read_only=true heights={}..={}", height - 1, height + 1);
    for pass in 1..=2 {
        for requested in height - 1..=height + 1 {
            let (block, tree) = tokio::try_join!(client.block(requested), client.tree(requested))?;
            let block_display = hex::encode(block.hash.iter().rev().copied().collect::<Vec<_>>());
            let parent_display =
                hex::encode(block.prev_hash.iter().rev().copied().collect::<Vec<_>>());
            println!(
                "{}",
                serde_json::json!({
                    "pass": pass,
                    "requested_height": requested,
                    "block_height": block.height,
                    "block_hash": block_display,
                    "parent_hash": parent_display,
                    "tree_height": tree.height,
                    "tree_network": tree.network,
                    "tree_hash": tree.hash,
                    "hash_match": block_display.eq_ignore_ascii_case(&tree.hash),
                    "ironwood_tree_bytes": tree.ironwood_tree.len() / 2,
                })
            );
            if !block_display.eq_ignore_ascii_case(&tree.hash) {
                match client.tree_by_hash(block.hash.as_slice().try_into()?).await {
                    Ok(by_hash) => println!(
                        "{}",
                        serde_json::json!({
                            "pass": pass,
                            "lookup": "by_hash",
                            "requested_height": requested,
                            "tree_height": by_hash.height,
                            "tree_network": by_hash.network,
                            "tree_hash": by_hash.hash,
                            "hash_match": block_display.eq_ignore_ascii_case(&by_hash.hash),
                            "same_frontier_as_height_lookup": by_hash.ironwood_tree == tree.ironwood_tree,
                        })
                    ),
                    Err(error) => println!(
                        "hash_lookup_failed height={requested} grpc_code={:?}",
                        zcash::rpc::failure_code(&error)
                    ),
                }
            }
        }
    }
    Ok(())
}
