//! Bounded HTTP client for the trusted, same-host FHE sidecar.
//! Signing keys remain in Rust; prepared and authorized sessions are single-use values.
use crate::contract::permit::{Permit, sign_permit_for_context};
use alloy::{
    dyn_abi::TypedData,
    primitives::{Address, B256, Bytes, Signature, U256},
    signers::local::PrivateKeySigner,
    transports::http::reqwest,
};
use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const DEADLINE: Duration = Duration::from_secs(130);
const MAX_RESPONSE: usize = 256 * 1024;

pub struct Client {
    http: reqwest::Client,
    base: reqwest::Url,
}

pub struct EncryptedInput {
    pub handle: B256,
    pub input_proof: Bytes,
}

#[derive(Clone, Copy)]
pub struct Scope {
    pub contract: Address,
    pub user: Address,
    pub handle: B256,
}

pub struct PreparedDecryption {
    scope: Scope,
    response: PreparedResponse,
}
pub struct AuthorizedDecryption {
    scope: Scope,
    session_id: String,
    expires_at: u64,
    signature: String,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct PreparedResponse {
    session_id: String,
    contract: Address,
    user: Address,
    handle: B256,
    expires_at: String,
    public_key: String,
    start_timestamp: String,
    typed_data: TypedData,
    digest: B256,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct EncryptResponse {
    handle: B256,
    input_proof: Bytes,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DecryptResponse {
    value: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorResponse {
    error: ErrorCode,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ErrorCode {
    code: String,
}

impl Client {
    /// Only explicit IPv4 loopback HTTP endpoints are accepted.
    pub fn new(base_url: &str) -> Result<Self> {
        let base = reqwest::Url::parse(base_url).map_err(|_| anyhow!("invalid_sidecar_url"))?;
        // Check the original authority too: URL normalization accepts alternate IP spellings.
        let authority = base_url
            .strip_prefix("http://")
            .and_then(|s| s.strip_suffix('/').or(Some(s)));
        let valid_authority = authority
            .and_then(|s| s.strip_prefix("127.0.0.1:"))
            .is_some_and(|p| {
                !p.is_empty()
                    && p.bytes().all(|b| b.is_ascii_digit())
                    && p.parse::<u16>().is_ok_and(|p| p != 0)
            });
        ensure!(
            valid_authority
                && base.scheme() == "http"
                && base.host_str() == Some("127.0.0.1")
                && base.username().is_empty()
                && base.password().is_none()
                && base.path() == "/"
                && base.query().is_none()
                && base.fragment().is_none(),
            "invalid_sidecar_url"
        );
        let http = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .retry(reqwest::retry::never())
            .connect_timeout(Duration::from_secs(5))
            .timeout(DEADLINE)
            .build()
            .map_err(|_| anyhow!("sidecar_client_failed"))?;
        Ok(Self { http, base })
    }

    pub async fn encrypt(
        &self,
        contract: Address,
        user: Address,
        value: u64,
    ) -> Result<EncryptedInput> {
        validate_addresses(contract, user)?;
        #[derive(Serialize)]
        struct Request {
            contract: Address,
            user: Address,
            value: String,
        }
        let response: EncryptResponse = self
            .post(
                "encrypt",
                &Request {
                    contract,
                    user,
                    value: value.to_string(),
                },
            )
            .await?;
        ensure!(
            !response.handle.is_zero() && !response.input_proof.is_empty(),
            "invalid_sidecar_response"
        );
        Ok(EncryptedInput {
            handle: response.handle,
            input_proof: response.input_proof,
        })
    }

    pub async fn prepare_decryption(&self, scope: Scope) -> Result<PreparedDecryption> {
        validate_scope(scope)?;
        #[derive(Serialize)]
        struct Request {
            contract: Address,
            user: Address,
            handle: B256,
        }
        let response = self
            .post(
                "decrypt-request",
                &Request {
                    contract: scope.contract,
                    user: scope.user,
                    handle: scope.handle,
                },
            )
            .await?;
        PreparedDecryption::validated(scope, response, now_ms()?)
    }

    pub async fn decrypt(&self, authorized: AuthorizedDecryption) -> Result<u64> {
        validate_expiry(authorized.expires_at, now_ms()?)?;
        #[derive(Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Request {
            session_id: String,
            contract: Address,
            user: Address,
            handle: B256,
            signature: String,
        }
        let response: DecryptResponse = self
            .post(
                "decrypt",
                &Request {
                    session_id: authorized.session_id,
                    contract: authorized.scope.contract,
                    user: authorized.scope.user,
                    handle: authorized.scope.handle,
                    signature: authorized.signature,
                },
            )
            .await?;
        decimal(&response.value).map_err(|_| anyhow!("invalid_sidecar_response"))
    }

    async fn post<T: DeserializeOwned>(
        &self,
        endpoint: &str,
        request: &impl Serialize,
    ) -> Result<T> {
        tokio::time::timeout(DEADLINE, async {
            let url = self
                .base
                .join(endpoint)
                .map_err(|_| anyhow!("invalid_sidecar_url"))?;
            let mut response = self
                .http
                .post(url)
                .json(request)
                .send()
                .await
                .map_err(|_| anyhow!("sidecar_transport_failed"))?;
            let status = response.status().as_u16();
            let content_type = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            ensure!(valid_content_type(content_type), "invalid_sidecar_response");
            ensure!(
                response
                    .content_length()
                    .is_none_or(|n| n <= MAX_RESPONSE as u64),
                "invalid_sidecar_response"
            );
            let mut bytes = Vec::new();
            while let Some(chunk) = response
                .chunk()
                .await
                .map_err(|_| anyhow!("sidecar_transport_failed"))?
            {
                ensure!(
                    chunk.len() <= MAX_RESPONSE.saturating_sub(bytes.len()),
                    "invalid_sidecar_response"
                );
                bytes.extend_from_slice(&chunk);
            }
            if status != 200 {
                return Err(response_error(status, &bytes));
            }
            serde_json::from_slice(&bytes).map_err(|_| anyhow!("invalid_sidecar_response"))
        })
        .await
        .map_err(|_| anyhow!("sidecar_timeout"))?
    }
}

impl PreparedDecryption {
    fn wallet_digest(&self, kms_context: U256) -> Result<B256> {
        validate_expiry(decimal(&self.response.expires_at)?, now_ms()?)?;
        crate::contract::permit::validate_permit_for_context(
            self.scope.contract,
            &Permit {
                public_key: self.response.public_key.clone(),
                start_timestamp: self.response.start_timestamp.clone(),
                typed_data: self.response.typed_data.clone(),
                digest: self.response.digest,
            },
            kms_context,
        )
    }

    /// Export only independently validated permit terms for the actual user's wallet.
    pub fn wallet_request(&self, kms_context: U256) -> Result<serde_json::Value> {
        self.wallet_digest(kms_context)?;
        Ok(serde_json::to_value(&self.response)?)
    }

    /// Restores an ephemeral session, not a signing key. Expired sessions require new consent.
    pub fn restore_wallet(scope: Scope, response: serde_json::Value) -> Result<Self> {
        Self::validated(scope, serde_json::from_value(response)?, now_ms()?)
    }

    pub fn authorize_wallet(
        self,
        signature: String,
        kms_context: U256,
    ) -> Result<AuthorizedDecryption> {
        let digest = self.wallet_digest(kms_context)?;
        let parsed: Signature = signature
            .parse()
            .map_err(|_| anyhow!("invalid wallet signature"))?;
        ensure!(
            parsed
                .recover_address_from_prehash(&digest)
                .is_ok_and(|user| user == self.scope.user),
            "wallet signature scope mismatch"
        );
        Ok(AuthorizedDecryption {
            scope: self.scope,
            session_id: self.response.session_id,
            expires_at: decimal(&self.response.expires_at)?,
            signature,
        })
    }

    fn validated(scope: Scope, response: PreparedResponse, now: u64) -> Result<Self> {
        validate_scope(scope)?;
        ensure!(
            scope.contract == response.contract
                && scope.user == response.user
                && scope.handle == response.handle,
            "sidecar_scope_mismatch"
        );
        ensure!(
            response.session_id.len() == 64
                && response
                    .session_id
                    .bytes()
                    .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),
            "invalid_sidecar_response"
        );
        validate_expiry(decimal(&response.expires_at)?, now)?;
        decimal(&response.start_timestamp)?;
        let key = response
            .public_key
            .strip_prefix("0x")
            .unwrap_or(&response.public_key);
        ensure!(
            !key.is_empty()
                && key.len() <= 8192
                && key.len() % 2 == 0
                && key.bytes().all(|b| b.is_ascii_hexdigit()),
            "invalid_sidecar_response"
        );
        ensure!(!response.digest.is_zero(), "invalid_sidecar_response");
        Ok(Self { scope, response })
    }

    /// `kms_context` must be read independently from the fixed Sepolia KMSVerifier
    /// over the caller's RPC connection, never copied from the sidecar permit.
    pub async fn authorize(
        self,
        signer: &PrivateKeySigner,
        kms_context: U256,
    ) -> Result<AuthorizedDecryption> {
        ensure!(
            signer.address() == self.scope.user,
            "permit_signer_mismatch"
        );
        let expires_at = decimal(&self.response.expires_at)?;
        validate_expiry(expires_at, now_ms()?)?;
        let digest = self.response.digest;
        let permit = Permit {
            public_key: self.response.public_key,
            start_timestamp: self.response.start_timestamp,
            typed_data: self.response.typed_data,
            digest,
        };
        let signature = sign_permit_for_context(signer, self.scope.contract, permit, kms_context)
            .await
            .map_err(|_| anyhow!("invalid_sidecar_permit"))?;
        let parsed: Signature = signature
            .parse()
            .map_err(|_| anyhow!("permit_signature_failed"))?;
        ensure!(
            signature.len() == 132
                && signature.starts_with("0x")
                && parsed
                    .recover_address_from_prehash(&digest)
                    .is_ok_and(|address| address == self.scope.user),
            "permit_signature_failed"
        );
        validate_expiry(expires_at, now_ms()?)?;
        Ok(AuthorizedDecryption {
            scope: self.scope,
            session_id: self.response.session_id,
            expires_at,
            signature,
        })
    }
}

fn validate_addresses(contract: Address, user: Address) -> Result<()> {
    ensure!(!contract.is_zero() && !user.is_zero(), "invalid_scope");
    Ok(())
}
fn validate_scope(scope: Scope) -> Result<()> {
    validate_addresses(scope.contract, scope.user)?;
    ensure!(
        scope.contract != scope.user && !scope.handle.is_zero(),
        "invalid_scope"
    );
    Ok(())
}
fn decimal(value: &str) -> Result<u64> {
    ensure!(
        !value.is_empty()
            && value.len() <= 20
            && value.bytes().all(|b| b.is_ascii_digit())
            && (value.len() == 1 || !value.starts_with('0')),
        "invalid_decimal"
    );
    value.parse().map_err(|_| anyhow!("invalid_decimal"))
}
fn now_ms() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|d| u64::try_from(d.as_millis()).ok())
        .ok_or_else(|| anyhow!("clock_unavailable"))
}
fn validate_expiry(expires: u64, now: u64) -> Result<()> {
    ensure!(
        expires > now && expires - now <= 300_000,
        "invalid_session_expiry"
    );
    Ok(())
}
fn valid_content_type(value: &str) -> bool {
    let mut parts = value.split(';');
    if !parts
        .next()
        .unwrap_or("")
        .trim()
        .eq_ignore_ascii_case("application/json")
    {
        return false;
    }
    match parts.next() {
        None => true,
        Some(p) => p.trim().eq_ignore_ascii_case("charset=utf-8") && parts.next().is_none(),
    }
}
fn response_error(status: u16, bytes: &[u8]) -> anyhow::Error {
    let Ok(body) = serde_json::from_slice::<ErrorResponse>(bytes) else {
        return anyhow!("invalid_sidecar_response");
    };
    let expected = match body.error.code.as_str() {
        "invalid_address" | "invalid_handle" | "invalid_user" | "invalid_uint64"
        | "invalid_request" | "invalid_json" | "invalid_session" | "invalid_signature" => 400,
        "invalid_host"
        | "origin_forbidden"
        | "session_scope_mismatch"
        | "signature_user_mismatch"
        | "sdk_acl_denied" => 403,
        "not_found" => 404,
        "method_not_allowed" => 405,
        "session_unavailable" => 410,
        "body_too_large" => 413,
        "unsupported_media_type" => 415,
        "encrypt_failed" | "permit_failed" | "decrypt_failed" | "sdk_failed" => 502,
        "sdk_unavailable" | "busy" | "session_capacity" => 503,
        "operation_timeout" => 504,
        _ => return anyhow!("invalid_sidecar_response"),
    };
    if expected != status {
        return anyhow!("invalid_sidecar_response");
    }
    anyhow!(body.error.code)
}

#[cfg(test)]
mod tests;
