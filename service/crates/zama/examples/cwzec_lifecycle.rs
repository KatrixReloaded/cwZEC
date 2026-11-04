//! Testnet contract acceptance only; never initiates a Zcash payout.
#[path = "zama_probe/sdk.rs"]
mod sdk;
#[path = "zama_probe/signing.rs"]
mod signing;

use alloy::{
    network::{EthereumWallet, TransactionBuilder},
    primitives::{Address, B256, Bytes, U256, keccak256},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::{TransactionReceipt, TransactionRequest},
    signers::local::PrivateKeySigner,
    sol,
    sol_types::{SolCall, SolValue},
};
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::{
    fs::{File, OpenOptions},
    io::Write,
    path::Path,
    time::Duration,
};

sol! {
    function decimals() external view returns (uint8);
    function confidentialMint(address to, bytes32 amount, bytes input_proof, bytes32 deposit_id) external returns (bytes32);
    function voidDeposit(bytes32 deposit_id) external;
    function consumed(bytes32 deposit_id) external view returns (uint8);
    function mintedAmounts(bytes32 deposit_id) external view returns (bytes32);
    function requestRedeem(bytes32 amount, bytes input_proof, bytes dest_ct) external returns (uint256);
    function nextRedeemId() external view returns (uint256);
    function redemptions(uint256 redeem_id) external view returns (address user, bytes32 amount, uint8 status, bytes32 zcash_txid);
    function reCreditRedeem(uint256 redeem_id) external;
    function markRedeemFulfilled(uint256 redeem_id, bytes32 zcash_txid) external;
    function confidentialBalanceOf(address account) external view returns (bytes32);
    function confidentialTotalSupply() external view returns (bytes32);
}

struct Probe {
    provider: DynProvider,
    owner: PrivateKeySigner,
    service: PrivateKeySigner,
    auditor: PrivateKeySigner,
    sdk: sdk::Sdk,
    contract: Address,
    progress: File,
    transactions: Vec<Value>,
    checks: Vec<&'static str>,
    handles: Vec<Value>,
}

#[tokio::main]
async fn main() {
    if let Err(error) = run().await {
        // RPC errors can contain request payloads or URL credentials.
        eprintln!("Contract probe stopped: {error}");
        std::process::exit(1);
    }
}

async fn run() -> Result<()> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../..")
        .canonicalize()?;
    let owner = signing::load_or_create(&root.join(".local/zama-probe.key"))?;
    let service = signing::load_or_create(&root.join(".local/cwzec-service.key"))?;
    let auditor = signing::load_or_create(&root.join(".local/cwzec-auditor.key"))?;
    let rpc = std::env::var("SEPOLIA_RPC_URL")
        .unwrap_or_else(|_| "https://ethereum-sepolia-rpc.publicnode.com".into());
    let client = alloy::transports::http::reqwest::Client::builder()
        .timeout(Duration::from_secs(30))
        .build()?;
    let provider = ProviderBuilder::new()
        .wallet(EthereumWallet::from(owner.clone()))
        .connect_reqwest(client, rpc.parse().context("invalid RPC URL")?)
        .erased();
    ensure!(
        provider
            .get_chain_id()
            .await
            .context("Sepolia chain check failed")?
            == 11155111,
        "RPC is not Sepolia"
    );
    ensure!(
        !provider
            .get_balance(owner.address())
            .await
            .context("gas balance lookup failed")?
            .is_zero(),
        "operator needs testnet gas"
    );
    let artifact: Value = serde_json::from_reader(
        File::open(root.join("contracts/artifacts/contracts/CWZEC.sol/CWZEC.json"))
            .context("compile CWZEC first")?,
    )?;
    let bytecode: Bytes = artifact["bytecode"]
        .as_str()
        .context("missing bytecode")?
        .parse()?;
    let mut code = bytecode.to_vec();
    code.extend((owner.address(), service.address(), auditor.address()).abi_encode());
    let progress = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(root.join(".local/cwzec-contract-progress.jsonl"))
        .context(
            "progress already exists or cannot be created; inspect before any new deployment",
        )?;
    let mut sdk = sdk::Sdk::start(&root, "contracts/scripts/live-sdk.mjs")?;
    sdk.request(json!({"command":"init","rpcUrl":rpc}))
        .await
        .context("live SDK initialization failed")?;
    let mut p = Probe {
        provider,
        owner,
        service,
        auditor,
        sdk,
        contract: Address::ZERO,
        progress,
        transactions: Vec::new(),
        checks: Vec::new(),
        handles: Vec::new(),
    };
    let deployment = p
        .send(
            "deployment",
            TransactionRequest::default()
                .from(p.owner.address())
                .with_deploy_code(code),
        )
        .await?;
    p.contract = deployment
        .contract_address
        .context("missing deployment address")?;
    let bytes = p.read(decimalsCall {}.abi_encode()).await?;
    ensure!(
        decimalsCall::abi_decode_returns(&bytes)? == 8,
        "wrong token decimals"
    );
    p.checks.push("eight_decimals");

    let minted = p.mint("initial_mint", "initial").await?;
    p.state("initial_mint", "initial", Some((minted, "initial")))
        .await?;
    p.checks.push("actual_initial_mint_matches_private_request");
    let outsider = PrivateKeySigner::random();
    let balance = p.balance().await?;
    let denied = decrypt(
        &mut p.sdk,
        &outsider,
        p.contract,
        vec![(balance, "initial")],
    )
    .await?;
    ensure!(
        denied["authorized"] == false && denied["reason"] == "sdk_acl_denied",
        "outsider balance denial not established"
    );
    p.checks.push("outsider_balance_sdk_acl_denied");

    let (over_id, over_amount) = p.burn("overburn", "over_balance").await?;
    p.state("overburn", "initial", Some((over_amount, "zero")))
        .await?;
    p.service_amount(over_amount, "zero").await?;
    p.recredit("overburn_recredit", over_id).await?;
    p.state("overburn_recredit", "initial", None).await?;
    p.checks
        .push("overburn_actual_zero_and_recredit_zero_preserve_balance_supply");

    let (partial_id, partial_amount) = p.burn("partial_burn", "partial").await?;
    p.state(
        "partial_burn",
        "remaining",
        Some((partial_amount, "partial")),
    )
    .await?;
    p.service_amount(partial_amount, "partial").await?;
    p.recredit("partial_recredit", partial_id).await?;
    p.state("partial_recredit", "initial", None).await?;
    p.checks.push("stored_actual_recredited_to_original_burner");

    let (exact_id, exact_amount) = p.burn("exact_burn", "initial").await?;
    p.state("exact_burn", "zero", Some((exact_amount, "initial")))
        .await?;
    p.service_amount(exact_amount, "initial").await?;
    // Explicitly synthetic: this demonstrates the operator-controlled transition,
    // not a Zcash payment or proof that a real payout is dead.
    let reference = keccak256("cwzec/contract-test/synthetic-payout-reference");
    p.transact(
        "fulfillment",
        markRedeemFulfilledCall {
            redeem_id: exact_id,
            zcash_txid: reference,
        }
        .abi_encode(),
    )
    .await?;
    let record = p.record(exact_id).await?;
    ensure!(
        record.status == 2 && record.zcash_txid == reference,
        "fulfillment record mismatch"
    );
    p.checks
        .push("fulfillment_recorded_with_explicitly_synthetic_reference");

    let void_id = deposit_id("void");
    p.transact(
        "void_deposit",
        voidDepositCall {
            deposit_id: void_id,
        }
        .abi_encode(),
    )
    .await?;
    ensure!(p.consumption(void_id).await? == 2, "void state mismatch");
    p.checks.push("deposit_void_recorded");

    let maximum = p.mint("maximum_mint", "maximum").await?;
    p.state("maximum_mint", "maximum", Some((maximum, "maximum")))
        .await?;
    let overflow = p.mint("overflow_mint", "overflow").await?;
    p.state("overflow_mint", "maximum", Some((overflow, "zero")))
        .await?;
    p.service_amount(overflow, "zero").await?;
    p.checks
        .push("overflow_mint_success_is_actual_zero_not_requested_issuance");

    let (reserved_id, reserved_amount) = p.burn("capacity_burn", "partial").await?;
    p.state(
        "capacity_burn",
        "maximum_remaining",
        Some((reserved_amount, "partial")),
    )
    .await?;
    let blocked = p.mint("reserved_capacity_mint", "overflow").await?;
    p.state(
        "reserved_capacity_mint",
        "maximum_remaining",
        Some((blocked, "zero")),
    )
    .await?;
    p.recredit("capacity_recredit", reserved_id).await?;
    p.state("capacity_recredit", "maximum", None).await?;
    p.checks
        .push("pending_burn_capacity_reserved_and_exact_recredit_restored");
    p.checks
        .push("auditor_decrypted_every_updated_supply_handle");
    p.checks.push("service_decrypted_actual_burn_handles");

    let evidence = json!({"chain_id":11155111,"contract":p.contract,"operator":p.owner.address(),
        "service":p.service.address(),"auditor":p.auditor.address(),"unauthorized_user":outsider.address(),
        "transactions":p.transactions,"checks":p.checks,"handles":p.handles,
        "synthetic_fulfillment_reference":reference,
        "versions":{"openzeppelin_confidential":"0.5.3","openzeppelin_contracts":"5.6.1","fhevm_solidity":"0.11.1",
            "solc":"0.8.28","live_relayer_sdk":"0.4.5","mock_relayer_sdk":"0.4.1","alloy":"1.8.3","rust":"1.92.0","node":"24.22.1"},
        "limits":"synthetic testnet token; no Zcash payout; one-block receipts, not finality; SDK ACL preflight denial; trusted operator controls fulfillment/recredit"});
    p.sdk.close().await?;
    let mut output = File::create(root.join(".local/cwzec-contract-evidence.json"))?;
    serde_json::to_writer_pretty(&mut output, &evidence)?;
    output.sync_all()?;
    println!("PASS: contract lifecycle and private amount comparisons verified");
    println!("Sanitized evidence: .local/cwzec-contract-evidence.json");
    Ok(())
}

impl Probe {
    async fn send(&mut self, stage: &str, tx: TransactionRequest) -> Result<TransactionReceipt> {
        println!("Submitting {stage}");
        let pending = self
            .provider
            .send_transaction(tx)
            .await
            .context("transaction submission failed; inspect nonce before retrying")?;
        let hash = *pending.tx_hash();
        writeln!(self.progress, "{}", json!({"stage":stage,"tx":hash}))?;
        self.progress.sync_all()?;
        println!("{stage}: {hash}");
        let receipt = pending
            .with_timeout(Some(Duration::from_secs(240)))
            .with_required_confirmations(1)
            .get_receipt()
            .await
            .context("receipt unresolved; do not automatically retry")?;
        ensure!(
            receipt.status() && receipt.from == self.owner.address(),
            "transaction receipt status/sender mismatch"
        );
        self.transactions.push(json!({"stage":stage,"tx":hash,"block":receipt.block_number,"contract":receipt.contract_address}));
        Ok(receipt)
    }

    async fn transact(&mut self, stage: &str, data: Vec<u8>) -> Result<()> {
        let receipt = self
            .send(
                stage,
                TransactionRequest::default()
                    .from(self.owner.address())
                    .to(self.contract)
                    .input(data.into()),
            )
            .await?;
        ensure!(
            receipt.to == Some(self.contract),
            "transaction target mismatch"
        );
        Ok(())
    }

    async fn read(&self, data: Vec<u8>) -> Result<Bytes> {
        self.provider
            .call(
                TransactionRequest::default()
                    .to(self.contract)
                    .input(data.into()),
            )
            .await
            .context("contract read failed")
    }

    async fn encrypt(&mut self, value: &str) -> Result<(B256, Bytes)> {
        let r = self.sdk.request(json!({"command":"encrypt","contract":self.contract,"user":self.owner.address(),"value":value})).await.context("input encryption failed")?;
        Ok((
            r["handle"].as_str().context("missing handle")?.parse()?,
            r["inputProof"].as_str().context("missing proof")?.parse()?,
        ))
    }

    async fn mint(&mut self, stage: &str, value: &str) -> Result<B256> {
        let (amount, input_proof) = self.encrypt(value).await?;
        let deposit_id = deposit_id(stage);
        self.transact(
            stage,
            confidentialMintCall {
                to: self.owner.address(),
                amount,
                input_proof,
                deposit_id,
            }
            .abi_encode(),
        )
        .await?;
        ensure!(
            self.consumption(deposit_id).await? == 1,
            "mint consumption state mismatch"
        );
        mintedAmountsCall::abi_decode_returns(
            &self
                .read(mintedAmountsCall { deposit_id }.abi_encode())
                .await?,
        )
        .context("decode actual mint handle")
    }

    async fn consumption(&self, deposit_id: B256) -> Result<u8> {
        consumedCall::abi_decode_returns(
            &self.read(consumedCall { deposit_id }.abi_encode()).await?,
        )
        .context("decode consumption")
    }

    async fn burn(&mut self, stage: &str, value: &str) -> Result<(U256, B256)> {
        let redeem_id = nextRedeemIdCall::abi_decode_returns(
            &self.read(nextRedeemIdCall {}.abi_encode()).await?,
        )?;
        let (amount, input_proof) = self.encrypt(value).await?;
        // Opaque synthetic destination fixture; no automated payout consumes it.
        let dest_ct = Bytes::from(keccak256("cwzec/contract-test/destination-fixture").to_vec());
        self.transact(
            stage,
            requestRedeemCall {
                amount,
                input_proof,
                dest_ct,
            }
            .abi_encode(),
        )
        .await?;
        let record = self.record(redeem_id).await?;
        ensure!(
            record.user == self.owner.address()
                && record.status == 1
                && record.zcash_txid == B256::ZERO,
            "redemption record mismatch"
        );
        Ok((redeem_id, record.amount))
    }

    async fn record(&self, redeem_id: U256) -> Result<redemptionsReturn> {
        redemptionsCall::abi_decode_returns(
            &self
                .read(redemptionsCall { redeem_id }.abi_encode())
                .await?,
        )
        .context("decode redemption")
    }

    async fn recredit(&mut self, stage: &str, redeem_id: U256) -> Result<()> {
        let before = self.record(redeem_id).await?;
        self.transact(stage, reCreditRedeemCall { redeem_id }.abi_encode())
            .await?;
        let after = self.record(redeem_id).await?;
        ensure!(
            after.status == 3
                && after.user == before.user
                && after.amount == before.amount
                && after.zcash_txid == B256::ZERO,
            "recredit mutated original terms"
        );
        Ok(())
    }

    async fn balance(&self) -> Result<B256> {
        confidentialBalanceOfCall::abi_decode_returns(
            &self
                .read(
                    confidentialBalanceOfCall {
                        account: self.owner.address(),
                    }
                    .abi_encode(),
                )
                .await?,
        )
        .context("decode balance")
    }

    async fn state(
        &mut self,
        stage: &str,
        expected: &str,
        actual: Option<(B256, &str)>,
    ) -> Result<()> {
        println!("Privately checking {stage}");
        let balance = self.balance().await?;
        let supply = confidentialTotalSupplyCall::abi_decode_returns(
            &self
                .read(confidentialTotalSupplyCall {}.abi_encode())
                .await?,
        )?;
        let mut checks = vec![(balance, expected)];
        if let Some(check) = actual {
            checks.push(check);
        }
        let owner = decrypt(&mut self.sdk, &self.owner, self.contract, checks)
            .await
            .context("owner decryption failed")?;
        ensure!(
            owner["authorized"] == true && owner["matchesExpected"] == true,
            "private balance/actual comparison failed"
        );
        let auditor = decrypt(
            &mut self.sdk,
            &self.auditor,
            self.contract,
            vec![(supply, expected)],
        )
        .await
        .context("auditor decryption failed")?;
        ensure!(
            auditor["authorized"] == true && auditor["matchesExpected"] == true,
            "private supply comparison failed"
        );
        self.handles.push(
            json!({"stage":stage,"balance":balance,"supply":supply,"actual":actual.map(|c|c.0)}),
        );
        Ok(())
    }

    async fn service_amount(&mut self, handle: B256, expected: &str) -> Result<()> {
        let r = decrypt(
            &mut self.sdk,
            &self.service,
            self.contract,
            vec![(handle, expected)],
        )
        .await
        .context("service decryption failed")?;
        ensure!(
            r["authorized"] == true && r["matchesExpected"] == true,
            "private service amount comparison failed"
        );
        Ok(())
    }
}

fn deposit_id(stage: &str) -> B256 {
    keccak256(format!("cwzec/contract-test/{stage}"))
}

async fn decrypt(
    sdk: &mut sdk::Sdk,
    signer: &PrivateKeySigner,
    contract: Address,
    checks: Vec<(B256, &str)>,
) -> Result<Value> {
    let checks: Vec<Value> = checks
        .into_iter()
        .map(|(handle, expected)| json!({"handle":handle,"expected":expected}))
        .collect();
    let request = sdk
        .request(
            json!({"command":"permit","contract":contract,"user":signer.address(),"checks":checks}),
        )
        .await?;
    let signature =
        signing::sign_permit(signer, contract, serde_json::from_value(request)?).await?;
    sdk.request(json!({"command":"decrypt","signature":signature}))
        .await
}
