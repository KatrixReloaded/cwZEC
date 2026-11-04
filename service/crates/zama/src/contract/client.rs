use super::bindings::*;
use alloy::{
    consensus::{
        SignableTransaction, Transaction, TxEip1559, TxEnvelope, transaction::SignerRecoverable,
    },
    eips::eip2718::{Decodable2718, Encodable2718},
    primitives::{Address, B256, Bytes, TxKind, U256, keccak256},
    providers::{DynProvider, Provider, ProviderBuilder},
    rpc::types::TransactionRequest,
    signers::{SignerSync, local::PrivateKeySigner},
    sol_types::SolCall,
    transports::http::reqwest,
};
use anyhow::{Result, anyhow, ensure};
use std::time::Duration;

/// Explicit caller-selected transaction terms. No nonce or fee filler is used.
#[derive(Clone, Copy, Debug)]
pub struct TransactionFees {
    pub nonce: u64,
    pub gas_limit: u64,
    pub max_fee_per_gas: u128,
    pub max_priority_fee_per_gas: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TransactionDomain {
    chain_id: u64,
    contract: Address,
}

/// Typed calldata bound to a domain and (for redemption) its actual user.
/// Intentionally has no Debug implementation: calldata includes opaque destination data.
pub struct BoundCall {
    domain: TransactionDomain,
    input: Bytes,
    user: Option<Address>,
}

/// Durable signed identity reconstructed from the exact broadcast bytes.
/// Intentionally has no Debug implementation.
pub struct SignedTransaction {
    raw: Bytes,
    hash: B256,
    sender: Address,
    nonce: u64,
    domain: TransactionDomain,
}

impl TransactionDomain {
    pub fn new(chain_id: u64, contract: Address) -> Result<Self> {
        ensure!(
            chain_id != 0 && !contract.is_zero(),
            "invalid transaction domain"
        );
        Ok(Self { chain_id, contract })
    }
    fn bind<C: SolCall>(&self, call: C, user: Option<Address>) -> BoundCall {
        BoundCall {
            domain: *self,
            input: call.abi_encode().into(),
            user,
        }
    }
    pub fn mint(&self, to: Address, amount: B256, proof: Bytes, deposit_id: B256) -> BoundCall {
        self.bind(
            confidentialMintCall {
                to,
                amount,
                proof,
                depositId: deposit_id,
            },
            None,
        )
    }
    pub fn void_deposit(&self, deposit_id: B256) -> BoundCall {
        self.bind(
            voidDepositCall {
                depositId: deposit_id,
            },
            None,
        )
    }
    pub fn request_redeem(
        &self,
        user: Address,
        amount: B256,
        proof: Bytes,
        dest_ct: Bytes,
    ) -> BoundCall {
        self.bind(
            requestRedeemCall {
                amount,
                proof,
                destCt: dest_ct,
            },
            Some(user),
        )
    }
    pub fn fulfill(&self, redeem_id: U256, zcash_txid: B256) -> BoundCall {
        self.bind(
            markRedeemFulfilledCall {
                redeemId: redeem_id,
                zcashTxid: zcash_txid,
            },
            None,
        )
    }
    pub fn recredit(&self, redeem_id: U256) -> BoundCall {
        self.bind(
            reCreditRedeemCall {
                redeemId: redeem_id,
            },
            None,
        )
    }
    pub fn sign_operator(
        &self,
        signer: &PrivateKeySigner,
        call: &BoundCall,
        fees: TransactionFees,
    ) -> Result<SignedTransaction> {
        ensure!(call.user.is_none(), "operator cannot sign requestRedeem");
        self.sign(signer, call, fees)
    }
    pub fn sign_user_redeem(
        &self,
        signer: &PrivateKeySigner,
        call: &BoundCall,
        fees: TransactionFees,
    ) -> Result<SignedTransaction> {
        ensure!(
            call.user == Some(signer.address()),
            "redemption user/signer mismatch"
        );
        self.sign(signer, call, fees)
    }
    fn sign(
        &self,
        signer: &PrivateKeySigner,
        call: &BoundCall,
        fees: TransactionFees,
    ) -> Result<SignedTransaction> {
        ensure!(call.domain == *self, "call domain mismatch");
        ensure!(call.input.len() <= 65536, "calldata bound exceeded");
        ensure!(
            fees.gas_limit != 0 && fees.max_fee_per_gas >= fees.max_priority_fee_per_gas,
            "invalid fee terms"
        );
        let tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce: fees.nonce,
            gas_limit: fees.gas_limit,
            max_fee_per_gas: fees.max_fee_per_gas,
            max_priority_fee_per_gas: fees.max_priority_fee_per_gas,
            to: TxKind::Call(self.contract),
            value: U256::ZERO,
            input: call.input.clone(),
            access_list: Default::default(),
        };
        let signature = signer
            .sign_hash_sync(&tx.signature_hash())
            .map_err(|_| anyhow!("transaction signing failed"))?;
        let envelope: TxEnvelope = tx.into_signed(signature).into();
        SignedTransaction::restore(
            self.chain_id,
            self.contract,
            signer.address(),
            envelope.encoded_2718().into(),
        )
    }
}

impl BoundCall {
    pub fn calldata(&self) -> &Bytes {
        &self.input
    }
    pub fn user(&self) -> Option<Address> {
        self.user
    }
}

fn valid_calldata(data: &[u8]) -> bool {
    fn matches<C: SolCall>(data: &[u8]) -> bool {
        C::abi_decode(data).is_ok_and(|call| call.abi_encode() == data)
    }
    matches::<confidentialMintCall>(data)
        || matches::<voidDepositCall>(data)
        || matches::<requestRedeemCall>(data)
        || matches::<markRedeemFulfilledCall>(data)
        || matches::<reCreditRedeemCall>(data)
}

impl SignedTransaction {
    /// Validate persisted bytes before recovery or rebroadcast; rejects trailing or noncanonical bytes.
    pub fn restore(chain_id: u64, contract: Address, sender: Address, raw: Bytes) -> Result<Self> {
        let domain = TransactionDomain::new(chain_id, contract)?;
        ensure!(raw.len() <= 66560, "signed transaction bound exceeded");
        let mut remaining = raw.as_ref();
        let envelope = TxEnvelope::decode_2718(&mut remaining)
            .map_err(|_| anyhow!("invalid signed transaction"))?;
        ensure!(
            remaining.is_empty() && envelope.encoded_2718() == raw.as_ref(),
            "noncanonical signed transaction"
        );
        ensure!(
            matches!(envelope, TxEnvelope::Eip1559(_)),
            "only EIP1559 supported"
        );
        ensure!(
            envelope.chain_id() == Some(chain_id),
            "signed chain mismatch"
        );
        ensure!(envelope.to() == Some(contract), "signed contract mismatch");
        ensure!(
            envelope.value().is_zero()
                && envelope.access_list().is_some_and(|list| list.0.is_empty()),
            "unsupported transaction terms"
        );
        ensure!(valid_calldata(envelope.input()), "unsupported CWZEC call");
        let recovered = envelope
            .recover_signer()
            .map_err(|_| anyhow!("invalid transaction signature"))?;
        ensure!(
            recovered == sender && !sender.is_zero(),
            "signed sender mismatch"
        );
        Ok(Self {
            hash: keccak256(&raw),
            nonce: envelope.nonce(),
            raw,
            sender,
            domain,
        })
    }
    pub fn matches_call(&self, call: &BoundCall) -> bool {
        let mut bytes = self.raw.as_ref();
        self.domain == call.domain
            && TxEnvelope::decode_2718(&mut bytes).is_ok_and(|tx| tx.input() == &call.input)
    }
    /// Public intent fields used to bind the durable deposit journal to signed bytes.
    pub fn mint_identity(&self) -> Option<(Address, B256)> {
        let mut bytes = self.raw.as_ref();
        let tx = TxEnvelope::decode_2718(&mut bytes).ok()?;
        let call = confidentialMintCall::abi_decode(tx.input()).ok()?;
        Some((call.to, call.depositId))
    }
    pub fn fulfillment_identity(&self) -> Option<(U256, B256)> {
        let mut bytes = self.raw.as_ref();
        let tx = TxEnvelope::decode_2718(&mut bytes).ok()?;
        let call = markRedeemFulfilledCall::abi_decode(tx.input()).ok()?;
        Some((call.redeemId, call.zcashTxid))
    }
    pub fn recredit_identity(&self) -> Option<U256> {
        let mut bytes = self.raw.as_ref();
        let tx = TxEnvelope::decode_2718(&mut bytes).ok()?;
        Some(reCreditRedeemCall::abi_decode(tx.input()).ok()?.redeemId)
    }
    pub fn void_identity(&self) -> Option<B256> {
        let mut bytes = self.raw.as_ref();
        let tx = TxEnvelope::decode_2718(&mut bytes).ok()?;
        Some(voidDepositCall::abi_decode(tx.input()).ok()?.depositId)
    }
    pub fn matches_user_redeem(&self, calldata: &Bytes) -> bool {
        let mut bytes = self.raw.as_ref();
        TxEnvelope::decode_2718(&mut bytes).is_ok_and(|tx| {
            tx.input() == calldata && requestRedeemCall::abi_decode(calldata).is_ok()
        })
    }
    pub fn raw(&self) -> &Bytes {
        &self.raw
    }
    pub fn hash(&self) -> B256 {
        self.hash
    }
    pub fn sender(&self) -> Address {
        self.sender
    }
    pub fn nonce(&self) -> u64 {
        self.nonce
    }
    pub fn chain_id(&self) -> u64 {
        self.domain.chain_id
    }
    pub fn contract(&self) -> Address {
        self.domain.contract
    }
}

/// Single-endpoint HTTP client. No signer, wallet, retry layer, or generic provider surface.
pub struct CwzecClient {
    pub(crate) provider: DynProvider,
    domain: TransactionDomain,
}
impl CwzecClient {
    pub async fn user_transaction(&self, hash: B256, user: Address) -> Result<SignedTransaction> {
        let tx = self
            .provider
            .get_transaction_by_hash(hash)
            .await
            .map_err(|_| anyhow!("user transaction lookup failed"))?
            .ok_or_else(|| anyhow!("user transaction unavailable"))?;
        SignedTransaction::restore(
            self.chain_id(),
            self.address(),
            user,
            tx.inner.inner().encoded_2718().into(),
        )
    }
    pub async fn connect(endpoint: &str, chain_id: u64, contract: Address) -> Result<Self> {
        let domain = TransactionDomain::new(chain_id, contract)?;
        let url = reqwest::Url::parse(endpoint).map_err(|_| anyhow!("invalid RPC endpoint"))?;
        ensure!(
            matches!(url.scheme(), "http" | "https")
                && url.username().is_empty()
                && url.password().is_none()
                && url.fragment().is_none(),
            "unsupported RPC endpoint"
        );
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .build()
            .map_err(|_| anyhow!("RPC transport initialization failed"))?;
        let provider = ProviderBuilder::new()
            .disable_recommended_fillers()
            .connect_reqwest(http, url)
            .erased();
        let client = Self { provider, domain };
        client.check_chain().await?;
        let code = client
            .provider
            .get_code_at(contract)
            .await
            .map_err(|_| anyhow!("contract code lookup failed"))?;
        ensure!(!code.is_empty(), "configured contract has no code");
        ensure!(
            client.decimals().await? == 8,
            "configured contract decimals mismatch"
        );
        Ok(client)
    }
    async fn check_chain(&self) -> Result<()> {
        ensure!(
            self.provider
                .get_chain_id()
                .await
                .map_err(|_| anyhow!("RPC chain lookup failed"))?
                == self.chain_id(),
            "RPC chain mismatch"
        );
        Ok(())
    }
    pub fn chain_id(&self) -> u64 {
        self.domain.chain_id
    }
    pub fn address(&self) -> Address {
        self.domain.contract
    }
    pub fn domain(&self) -> TransactionDomain {
        self.domain
    }
    pub async fn latest_nonce(&self, sender: Address) -> Result<u64> {
        self.provider
            .get_transaction_count(sender)
            .await
            .map_err(|_| anyhow!("latest nonce lookup failed"))
    }
    /// RPC fee suggestions only; caller must apply its own spending limits before signing.
    pub async fn suggested_fees(&self) -> Result<(u128, u128)> {
        let fees = self
            .provider
            .estimate_eip1559_fees()
            .await
            .map_err(|_| anyhow!("fee estimation failed"))?;
        Ok((fees.max_fee_per_gas, fees.max_priority_fee_per_gas))
    }
    /// Gas estimate only. The actual redemption caller must be the bound user.
    pub async fn estimate_gas(&self, call: &BoundCall, sender: Address) -> Result<u64> {
        ensure!(call.domain == self.domain, "call domain mismatch");
        ensure!(
            call.user.is_none_or(|user| user == sender),
            "redemption user/sender mismatch"
        );
        self.provider
            .estimate_gas(
                TransactionRequest::default()
                    .from(sender)
                    .to(self.address())
                    .input(call.input.clone().into()),
            )
            .await
            .map_err(|_| anyhow!("gas estimation failed"))
    }
    pub async fn pending_nonce(&self, sender: Address) -> Result<u64> {
        self.provider
            .get_transaction_count(sender)
            .pending()
            .await
            .map_err(|_| anyhow!("pending nonce lookup failed"))
    }
    /// Broadcast exact previously persisted bytes. A timeout is ambiguous and must not trigger a fresh nonce.
    pub async fn broadcast(&self, signed: &SignedTransaction) -> Result<B256> {
        ensure!(signed.domain == self.domain, "broadcast domain mismatch");
        self.check_chain().await?;
        let pending = self
            .provider
            .send_raw_transaction(signed.raw())
            .await
            .map_err(|_| anyhow!("broadcast unresolved; reconcile saved transaction hash"))?;
        ensure!(
            *pending.tx_hash() == signed.hash(),
            "RPC broadcast hash mismatch"
        );
        Ok(signed.hash())
    }
    async fn read<C: SolCall>(&self, call: C) -> Result<C::Return> {
        let bytes = self
            .provider
            .call(
                TransactionRequest::default()
                    .to(self.address())
                    .input(call.abi_encode().into()),
            )
            .await
            .map_err(|_| anyhow!("CWZEC read failed"))?;
        C::abi_decode_returns(&bytes).map_err(|_| anyhow!("invalid CWZEC read response"))
    }
    pub async fn consumed(&self, deposit_id: B256) -> Result<DepositStatus> {
        DepositStatus::try_from(
            self.read(consumedCall {
                depositId: deposit_id,
            })
            .await?,
        )
    }
    pub async fn minted_amount(&self, deposit_id: B256) -> Result<B256> {
        self.read(mintedAmountsCall {
            depositId: deposit_id,
        })
        .await
    }
    pub async fn redemption(&self, redeem_id: U256) -> Result<Redemption> {
        let r = self
            .read(redemptionsCall {
                redeemId: redeem_id,
            })
            .await?;
        Ok(Redemption {
            user: r.user,
            amount: r.amount,
            status: RedemptionStatus::try_from(r.status)?,
            zcash_txid: r.zcashTxid,
        })
    }
    pub async fn balance(&self, account: Address) -> Result<B256> {
        self.read(confidentialBalanceOfCall { account }).await
    }
    pub async fn supply(&self) -> Result<B256> {
        self.read(confidentialTotalSupplyCall {}).await
    }
    pub async fn minter_role(&self) -> Result<B256> {
        self.read(MINTER_ROLECall {}).await
    }
    pub async fn default_admin_role(&self) -> Result<B256> {
        self.read(DEFAULT_ADMIN_ROLECall {}).await
    }
    pub async fn has_role(&self, role: B256, account: Address) -> Result<bool> {
        self.read(hasRoleCall { role, account }).await
    }
    pub async fn role_admin(&self, role: B256) -> Result<B256> {
        self.read(getRoleAdminCall { role }).await
    }
    pub async fn service(&self) -> Result<Address> {
        self.read(serviceCall {}).await
    }
    pub async fn auditor(&self) -> Result<Address> {
        self.read(auditorCall {}).await
    }
    pub async fn next_redeem_id(&self) -> Result<U256> {
        self.read(nextRedeemIdCall {}).await
    }
    pub async fn decimals(&self) -> Result<u8> {
        self.read(decimalsCall {}).await
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DepositStatus {
    None,
    Minted,
    Refunded,
}
impl TryFrom<u8> for DepositStatus {
    type Error = anyhow::Error;
    fn try_from(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::None),
            1 => Ok(Self::Minted),
            2 => Ok(Self::Refunded),
            _ => Err(anyhow!("invalid deposit status")),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RedemptionStatus {
    None,
    Pending,
    Fulfilled,
    Recredited,
}
impl TryFrom<u8> for RedemptionStatus {
    type Error = anyhow::Error;
    fn try_from(v: u8) -> Result<Self> {
        match v {
            0 => Ok(Self::None),
            1 => Ok(Self::Pending),
            2 => Ok(Self::Fulfilled),
            3 => Ok(Self::Recredited),
            _ => Err(anyhow!("invalid redemption status")),
        }
    }
}
/// Encrypted amount is a handle, never a settlement amount or payment assertion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Redemption {
    pub user: Address,
    pub amount: B256,
    pub status: RedemptionStatus,
    pub zcash_txid: B256,
}
