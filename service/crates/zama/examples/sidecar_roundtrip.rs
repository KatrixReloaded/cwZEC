//! One isolated HTTP/SDK input transaction on the existing FheProbe. No bridge workflow.
#[path = "zama_probe/signing.rs"]
mod signing;
use alloy::{
    network::EthereumWallet,
    primitives::{Address, B256, U256, address},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol,
    sol_types::SolCall,
    transports::http::reqwest,
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{fs::OpenOptions, io::Write, path::Path, process::Stdio, time::Duration};
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
};
use zama::fhe::{Client, Scope};

const CONTRACT: Address = address!("e35ce0d870f99e53b93e59f22e9a4574bf7c1c49");
const ACL: Address = address!("f0Ffdc93b7E186bC2f8CB3dAA75D86d1930A433D");
const KMS: Address = address!("bE0E383937d564D7FF0BC3b46c51f0bF8d5C311A");
// Deliberately above JS's exact Number range. Never written to logs or evidence.
const INPUT: u64 = 9_007_199_254_740_993;
sol! {
    function submit(bytes32 input, bytes proof);
    function owner() external view returns (address);
    function result() external view returns (bytes32);
    function persistAllowed(bytes32 handle, address account) external view returns (bool);
    function isAllowedForDecryption(bytes32 handle) external view returns (bool);
    function getCurrentKmsContextId() external view returns (uint256);
}

struct Sidecar {
    child: Child,
    url: String,
}
impl Sidecar {
    async fn start(root: &Path, rpc: &str) -> Result<Self> {
        let node =
            std::env::var("NODE_BIN").context("NODE_BIN must identify the selected runtime")?;
        let mut child = Command::new(node)
            .arg(root.join("sidecar/src/main.ts"))
            .env_clear()
            .env("SEPOLIA_RPC_URL", rpc)
            .env("SIDECAR_PORT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .kill_on_drop(true)
            .spawn()
            .context("start isolated HTTP sidecar")?;
        let mut reader = BufReader::new(child.stdout.take().context("sidecar stdout missing")?);
        let mut line = String::new();
        tokio::time::timeout(Duration::from_secs(150), reader.read_line(&mut line))
            .await
            .context("sidecar startup timed out")?
            .context("read sidecar readiness")?;
        let port: u16 = line
            .trim()
            .strip_prefix("Sidecar ready on 127.0.0.1:")
            .context("sidecar initialization failed")?
            .parse()
            .context("invalid sidecar port")?;
        ensure!(port != 0, "invalid sidecar port");
        Ok(Self {
            child,
            url: format!("http://127.0.0.1:{port}"),
        })
    }
    async fn stop(mut self) -> Result<()> {
        self.child.kill().await.context("stop sidecar process")
    }
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        eprintln!("HTTP round trip stopped: {error}");
        std::process::exit(1);
    }
}
async fn call(
    provider: &DynProvider,
    to: Address,
    data: Vec<u8>,
) -> Result<alloy::primitives::Bytes> {
    provider
        .call(TransactionRequest::default().to(to).input(data.into()))
        .await
        .context("read Sepolia state")
}
async fn context(provider: &DynProvider) -> Result<U256> {
    getCurrentKmsContextIdCall::abi_decode_returns(
        &call(provider, KMS, getCurrentKmsContextIdCall {}.abi_encode()).await?,
    )
    .context("decode KMS context")
}
async fn allowed(provider: &DynProvider, handle: B256, account: Address) -> Result<bool> {
    persistAllowedCall::abi_decode_returns(
        &call(
            provider,
            ACL,
            persistAllowedCall { handle, account }.abi_encode(),
        )
        .await?,
    )
    .context("decode ACL permission")
}
async fn run() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    ensure!(
        root.join(".local/sidecar-sdk-compatibility.json").is_file(),
        "run the read-only compatibility gate first"
    );
    let key = root.join(".local/zama-probe.key");
    ensure!(key.is_file(), "existing probe test key missing");
    let signer = signing::load_or_create(&key)?;
    let rpc = std::env::var("SEPOLIA_RPC_URL")
        .unwrap_or_else(|_| "https://ethereum-sepolia-rpc.publicnode.com".into());
    let transport = reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()
        .context("create RPC transport")?;
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(signer.clone()))
        .connect_reqwest(transport, rpc.parse().context("invalid RPC URL")?)
        .erased();
    ensure!(
        provider.get_chain_id().await.context("check chain")? == 11155111,
        "RPC chain mismatch"
    );
    let owner =
        ownerCall::abi_decode_returns(&call(&provider, CONTRACT, ownerCall {}.abi_encode()).await?)
            .context("decode owner")?;
    ensure!(owner == signer.address(), "probe owner mismatch");
    let before = resultCall::abi_decode_returns(
        &call(&provider, CONTRACT, resultCall {}.abi_encode()).await?,
    )
    .context("decode existing handle")?;
    ensure!(
        !before.is_zero() && allowed(&provider, before, owner).await?,
        "existing probe state mismatch"
    );
    println!("Existing probe and owner verified; starting isolated HTTP sidecar");
    let process = Sidecar::start(&root, &rpc).await?;
    let client = Client::new(&process.url)?;
    let progress_path = root.join(".local/sidecar-http-progress.json");
    let tx: B256;
    if progress_path.exists() {
        let saved: Value =
            serde_json::from_slice(&std::fs::read(&progress_path).context("read HTTP progress")?)
                .context("invalid HTTP progress")?;
        tx = saved["tx"]
            .as_str()
            .context("previous submission uncertain; inspect owner nonce before any retry")?
            .parse()
            .context("invalid saved transaction")?;
        println!("Resuming recorded HTTP transaction: {tx}");
    } else {
        println!("Encrypting through Rust and HTTP; submitting one probe input");
        let input = client
            .encrypt(CONTRACT, owner, INPUT)
            .await
            .context("HTTP encryption failed")?;
        // Reserve the attempt before broadcasting. An ambiguous no-hash state is
        // deliberately not retried automatically; no durable bridge journal here.
        let mut progress = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&progress_path)
            .context("reserve isolated input submission")?;
        progress.write_all(b"{\"state\":\"submission_started\"}\n")?;
        progress.sync_all()?;
        let pending = provider
            .send_transaction(
                TransactionRequest::default().to(CONTRACT).input(
                    submitCall {
                        input: input.handle,
                        proof: input.input_proof,
                    }
                    .abi_encode()
                    .into(),
                ),
            )
            .await
            .context("input submission uncertain; inspect progress and owner nonce")?;
        tx = *pending.tx_hash();
        let saved = json!({"tx":tx,"contract":CONTRACT,"owner":owner,"input_handle":input.handle});
        let tmp = root.join(".local/sidecar-http-progress.tmp");
        let mut file = std::fs::File::create(&tmp)?;
        file.write_all(&serde_json::to_vec_pretty(&saved)?)?;
        file.sync_all()?;
        std::fs::rename(tmp, &progress_path)?;
        println!("HTTP input transaction: {tx}");
    }
    let receipt = tokio::time::timeout(Duration::from_secs(180), async {
        loop {
            if let Some(receipt) = provider
                .get_transaction_receipt(tx)
                .await
                .context("read input receipt")?
            {
                break Ok::<_, anyhow::Error>(receipt);
            }
            tokio::time::sleep(Duration::from_secs(2)).await;
        }
    })
    .await
    .context("input receipt unresolved; rerun resumes the same transaction")??;
    ensure!(
        receipt.status() && receipt.from == owner && receipt.to == Some(CONTRACT),
        "input receipt mismatch or revert"
    );
    let handle = resultCall::abi_decode_returns(
        &call(&provider, CONTRACT, resultCall {}.abi_encode()).await?,
    )
    .context("decode result handle")?;
    ensure!(
        !handle.is_zero()
            && allowed(&provider, handle, owner).await?
            && allowed(&provider, handle, CONTRACT).await?,
        "result ACL missing"
    );
    ensure!(
        !isAllowedForDecryptionCall::abi_decode_returns(
            &call(
                &provider,
                ACL,
                isAllowedForDecryptionCall { handle }.abi_encode()
            )
            .await?
        )
        .context("decode public ACL")?,
        "result unexpectedly public"
    );
    let scope = Scope {
        contract: CONTRACT,
        user: owner,
        handle,
    };
    let authorized = client
        .prepare_decryption(scope)
        .await
        .context("prepare HTTP permit")?
        .authorize(&signer, context(&provider).await?)
        .await
        .context("validate and sign HTTP permit")?;
    ensure!(
        client
            .decrypt(authorized)
            .await
            .context("authorized HTTP decryption")?
            == INPUT + 1,
        "private on-chain result mismatch"
    );
    println!("On-chain input consumed; authorized private result matched");
    let outsider = PrivateKeySigner::random();
    ensure!(
        !allowed(&provider, handle, outsider.address()).await?,
        "outsider unexpectedly authorized"
    );
    let denied_scope = Scope {
        user: outsider.address(),
        ..scope
    };
    let denied = client
        .prepare_decryption(denied_scope)
        .await
        .context("prepare outsider permit")?
        .authorize(&outsider, context(&provider).await?)
        .await
        .context("sign valid outsider permit")?;
    let error = client
        .decrypt(denied)
        .await
        .err()
        .context("unauthorized decryption succeeded")?;
    ensure!(
        error.to_string() == "sdk_acl_denied",
        "unauthorized flow failed outside SDK user ACL preflight"
    );
    println!("Unauthorized supported flow denied by SDK user ACL preflight");
    let old = client
        .prepare_decryption(scope)
        .await
        .context("prepare pre-restart session")?
        .authorize(&signer, context(&provider).await?)
        .await
        .context("sign pre-restart permit")?;
    process.stop().await?;
    let restarted = Sidecar::start(&root, &rpc).await?;
    let fresh_client = Client::new(&restarted.url)?;
    let error = fresh_client
        .decrypt(old)
        .await
        .err()
        .context("old session survived sidecar restart")?;
    ensure!(
        error.to_string() == "session_unavailable",
        "unexpected restart denial layer"
    );
    let fresh = fresh_client
        .prepare_decryption(scope)
        .await
        .context("prepare fresh session")?
        .authorize(&signer, context(&provider).await?)
        .await
        .context("sign fresh permit")?;
    ensure!(
        fresh_client
            .decrypt(fresh)
            .await
            .context("decrypt after restart")?
            == INPUT + 1,
        "fresh authorization mismatch"
    );
    restarted.stop().await?;
    std::fs::write(
        root.join(".local/sidecar-http-evidence.json"),
        serde_json::to_vec_pretty(&json!({
            "sdk":"3.7.1","backend":"0.13.2","chain_id":11155111,"contract":CONTRACT,
            "tx":tx,"block":receipt.block_number,"result_handle":handle,"owner":owner,"unauthorized_user":outsider.address(),
            "checks":["rust_http_sdk_encryption","onchain_input_consumed","private_authorized_comparison","valid_permit_sdk_acl_preflight_denial","restart_session_unavailable","fresh_authorization_after_restart"],
            "transaction_limit":1,"receipt_confirmations":1
        }))?,
    )?;
    println!("PASS: Rust → HTTP → SDK round trip, private result, ACL denial, restart recovery");
    Ok(())
}
