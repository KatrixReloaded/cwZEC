//! Bounded HTTP facade over persisted bridge state and existing admission barriers.
use super::{authorize_key, txid_text};
use crate::{
    core::{
        bridge_types::{BridgeCheckpointPair, DepositState},
        deposit_id,
        destination::DestinationKey,
        make_memo, parse_memo,
        redeem_types::{MIN_PAYOUT_ZAT, REDEEM_FEE_ZAT, RedeemState},
    },
    store::Store,
};
use alloy::primitives::{Address, B256, U256};
use axum::{
    Json, Router,
    extract::{DefaultBodyLimit, Path, Query, Request, State, rejection::JsonRejection},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};
use zama::{contract::CwzecClient, fhe, watcher::Event};
use zcash::rpc::LightwalletClient;

pub struct BridgeApi {
    deposit_address: String,
    admin_key: String,
    store: tokio::sync::Mutex<Store>,
    contract: Arc<CwzecClient>,
    fhe: Arc<fhe::Client>,
    destination_key: Arc<DestinationKey>,
    zcash_endpoint: String,
    ready: Arc<AtomicBool>,
    payment_lock: Arc<tokio::sync::Mutex<()>>,
}
impl BridgeApi {
    pub fn new(
        store: Store,
        contract: Arc<CwzecClient>,
        fhe: Arc<fhe::Client>,
        destination_key: Arc<DestinationKey>,
        zcash_endpoint: String,
        deposit_address: String,
        admin_key: String,
        ready: Arc<AtomicBool>,
    ) -> Self {
        Self {
            deposit_address,
            admin_key,
            store: tokio::sync::Mutex::new(store),
            contract,
            fhe,
            destination_key,
            zcash_endpoint,
            ready,
            payment_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }
    pub fn with_payment_lock(mut self, payment_lock: Arc<tokio::sync::Mutex<()>>) -> Self {
        self.payment_lock = payment_lock;
        self
    }
}
#[derive(Clone, Copy)]
struct ApiError(StatusCode, &'static str);
type Reply = Result<Json<Value>, ApiError>;
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error":self.1}))).into_response()
    }
}
fn invalid() -> ApiError {
    ApiError(StatusCode::BAD_REQUEST, "invalid_request")
}
fn unavailable() -> ApiError {
    ApiError(StatusCode::SERVICE_UNAVAILABLE, "service_unavailable")
}
fn conflict() -> ApiError {
    ApiError(StatusCode::CONFLICT, "operation_blocked_or_terms_conflict")
}
fn missing() -> ApiError {
    ApiError(StatusCode::NOT_FOUND, "not_found")
}
fn body<T>(v: Result<Json<T>, JsonRejection>) -> Result<T, ApiError> {
    v.map(|Json(v)| v).map_err(|e| {
        if e.status() == StatusCode::PAYLOAD_TOO_LARGE {
            ApiError(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large")
        } else {
            invalid()
        }
    })
}

pub fn bridge_router(api: Arc<BridgeApi>) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/memo", get(memo))
        .route("/v1/deposits/{txid}", get(deposits_tx))
        .route("/v1/deposits", get(deposits_recipient))
        .route("/v1/redeem/prepare", post(prepare))
        .route("/v1/redeems/{id}", get(redeem))
        .route("/v1/admin/status", get(status))
        .route("/v1/admin/reconcile", get(reconcile))
        .route("/v1/admin/recovery", get(recovery).post(request_recovery))
        .route("/v1/admin/refund", post(refund))
        .route("/v1/admin/recredit", post(recredit))
        .route("/v1/admin/payout", post(disabled_payout))
        .route("/v1/payouts/{id}", get(payout))
        .fallback(|| async { missing().into_response() })
        .layer(DefaultBodyLimit::max(8192))
        .layer(middleware::from_fn_with_state(api.clone(), guard))
        .with_state(api)
}
async fn guard(State(s): State<Arc<BridgeApi>>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    if (path.starts_with("/v1/admin/")
        || path == "/v1/deposits"
        || path.starts_with("/v1/payouts/"))
        && authorize_key(&s.admin_key, req.headers()).is_err()
    {
        return ApiError(StatusCode::UNAUTHORIZED, "unauthorized").into_response();
    }
    match tokio::time::timeout(Duration::from_secs(30), next.run(req)).await {
        Ok(response) if response.status() == StatusCode::METHOD_NOT_ALLOWED => {
            ApiError(StatusCode::METHOD_NOT_ALLOWED, "method_not_allowed").into_response()
        }
        Ok(response)
            if (response.status().is_client_error() || response.status().is_server_error())
                && response
                    .headers()
                    .get(axum::http::header::CONTENT_TYPE)
                    .is_none_or(|v| v != "application/json") =>
        {
            ApiError(response.status(), "invalid_request").into_response()
        }
        Ok(response) => response,
        Err(_) => ApiError(StatusCode::REQUEST_TIMEOUT, "request_timeout").into_response(),
    }
}
async fn info(State(s): State<Arc<BridgeApi>>) -> Reply {
    let health = s
        .store
        .lock()
        .await
        .http_health()
        .await
        .map_err(|_| unavailable())?;
    let ready = s.ready.load(Ordering::Acquire)
        && health["halted"] == false
        && health["report_valid"] == true;
    Ok(Json(
        json!({"network":"testnet","pool":"ironwood","attested":false,"deposit_address":s.deposit_address,"contract":s.contract.address(),"chain_id":s.contract.chain_id().to_string(),"zcash_confirmations":3,"evm_confirmations":2,"memo_version":"cwzec:v1","memo_format":"cwzec:v1:11155111:{lowercase_0x_recipient}:{checksum}","memo_checksum":"first four SHA-256 bytes of preceding ASCII body, lowercase hex","fixed_fee_zat":REDEEM_FEE_ZAT.to_string(),"minimum_redeem_zat":(REDEEM_FEE_ZAT+MIN_PAYOUT_ZAT).to_string(),"scanner_ready":ready,"persisted":health}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Memo {
    recipient: String,
}
async fn memo(v: Result<Query<Memo>, axum::extract::rejection::QueryRejection>) -> Reply {
    let Query(request) = v.map_err(|_| invalid())?;
    Ok(Json(
        json!({"memo":make_memo(&request.recipient).map_err(|_|invalid())?}),
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Prepare {
    operation_id: String,
    user: Address,
    amount: String,
    zcash_address: String,
    chain_id: String,
    contract: Address,
}
fn decimal_u64(s: &str) -> Result<u64, ApiError> {
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    s.parse().map_err(|_| invalid())
}
fn redeem_id(s: &str) -> Result<U256, ApiError> {
    if s.is_empty() || (s.len() > 1 && s.starts_with('0')) || !s.bytes().all(|b| b.is_ascii_digit())
    {
        return Err(invalid());
    }
    U256::from_str_radix(s, 10).map_err(|_| invalid())
}
async fn prepare(
    State(s): State<Arc<BridgeApi>>,
    v: Result<Json<Prepare>, JsonRejection>,
) -> Reply {
    let p = body(v)?;
    let amount = decimal_u64(&p.amount)?;
    if amount < REDEEM_FEE_ZAT + MIN_PAYOUT_ZAT
        || p.user.is_zero()
        || p.operation_id.is_empty()
        || p.operation_id.len() > 128
    {
        return Err(invalid());
    }
    if decimal_u64(&p.chain_id)? != s.contract.chain_id() || p.contract != s.contract.address() {
        return Err(conflict());
    }
    // A request disconnect cannot cancel accepted durable preparation.
    tokio::spawn(async move {
        let saved = crate::core::user_prepare::prepare_redeem(
            &mut *s.store.lock().await, &s.contract, &s.fhe, &s.destination_key,
            &p.operation_id, p.user, amount, &p.zcash_address,
        ).await.map_err(|e| match e.to_string().as_str() {
            "invalid user request" | "invalid amount" | "invalid destination" => invalid(),
            "request domain mismatch" | "request ID terms changed" | "user request identity already bound" => conflict(),
            _ => unavailable(),
        })?;
        if let Some(signed) = saved.signed {
            return Ok(Json(json!({"operation_id":saved.id,"transaction_hash":signed.hash(),"already_submitted":true,"informational_only":true})));
        }
        Ok(Json(json!({"operation_id":saved.id,"transaction":{"from":saved.user,"to":s.contract.address(),"chainId":format!("0x{:x}",s.contract.chain_id()),"value":"0x0","data":saved.calldata},"already_submitted":false,"fixed_fee_zat":REDEEM_FEE_ZAT.to_string(),"minimum_redeem_zat":(REDEEM_FEE_ZAT+MIN_PAYOUT_ZAT).to_string(),"informational_only":true})))
    }).await.map_err(|_|unavailable())?
}

async fn deposit_records(
    store: &mut Store,
    txid: Option<[u8; 32]>,
    recipient: Option<Address>,
) -> Reply {
    let height = store
        .load()
        .await
        .map_err(|_| unavailable())?
        .checkpoint
        .height;
    let bridge = store.bridge_deposits().await.map_err(|_| unavailable())?;
    let events = store.zama_events().await.map_err(|_| unavailable())?;
    let evm_tip = store
        .zama_cursor()
        .await
        .map_err(|_| unavailable())?
        .map_or(0, |c| c.tip.number);
    let refunds = store.bridge_refunds().await.map_err(|_| unavailable())?;
    let mut records = Vec::new();
    for n in store.deposits().await.map_err(|_| unavailable())? {
        if n.internal
            || txid.is_some_and(|t| t != n.id.txid)
            || recipient.is_some_and(|a| parse_memo(&n.memo).ok() != Some(a.into_array()))
        {
            continue;
        }
        let id = B256::from(deposit_id(&n.rho, &n.rseed));
        let d = bridge.iter().find(|d| d.canonical && d.note_id == n.id);
        let confirmations = height.checked_sub(n.mined_height).map_or(0, |n| n + 1);
        let stage = match d.map(|d| d.state) {
            Some(DepositState::Minted) => "minted",
            Some(DepositState::MintPending) => "minting",
            Some(DepositState::RefundPending) => "refund_pending",
            Some(DepositState::Voided) => "voided",
            Some(DepositState::Mismatch) => "failed",
            _ if confirmations < 3 => "awaiting_confirmations",
            _ => "detected",
        };
        let mint = if let Some(d) = d {
            store
                .zama_transaction(&d.operation_id)
                .await
                .map_err(|_| unavailable())?
                .map(|t| t.signed.hash())
        } else {
            None
        };
        let mint_event = events.iter().find(
            |e| matches!(e.event, Event::DepositMinted { deposit_id, .. } if deposit_id == id),
        );
        let refund = if let Some(f) = refunds.iter().find(|f| f.deposit_id == id) {
            Some(
                json!({"state":format!("{:?}",f.state),"version":f.version.to_string(),"payment":payment(store,&f.operation_id).await?}),
            )
        } else {
            None
        };
        records.push(json!({"refund":refund,"evm_confirmations":mint_event.and_then(|e|evm_tip.checked_sub(e.block.number)).map_or(0,|n|n+1),"required_evm_confirmations":2,"txid":txid_text(n.id.txid),"action_index":n.id.action_index,"deposit_id":id,"status":stage,"confirmations":confirmations,"required_confirmations":3,"mint_transaction":mint,"version":d.map(|d|d.version.to_string())}));
    }
    Ok(Json(json!({"deposits":records})))
}
async fn deposits_tx(State(s): State<Arc<BridgeApi>>, Path(txid): Path<String>) -> Reply {
    if txid.len() != 64 {
        return Err(invalid());
    }
    let mut bytes: [u8; 32] = hex::decode(txid)
        .map_err(|_| invalid())?
        .try_into()
        .map_err(|_| invalid())?;
    bytes.reverse();
    deposit_records(&mut *s.store.lock().await, Some(bytes), None).await
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recipient {
    recipient: Address,
}
async fn deposits_recipient(
    State(s): State<Arc<BridgeApi>>,
    query: Result<Query<Recipient>, axum::extract::rejection::QueryRejection>,
) -> Reply {
    let Query(q) = query.map_err(|_| invalid())?;
    deposit_records(&mut *s.store.lock().await, None, Some(q.recipient)).await
}

async fn payment(store: &mut Store, id: &str) -> Result<Option<Value>, ApiError> {
    let Some(a) = store.get_attempt(id).await.map_err(|_| unavailable())? else {
        return Ok(None);
    };
    let height = store
        .load()
        .await
        .map_err(|_| unavailable())?
        .checkpoint
        .height;
    let notes = store.deposits().await.map_err(|_| unavailable())?;
    let confirmations = a
        .observed_height
        .and_then(|h| height.checked_sub(h))
        .map_or(0, |n| n + 1);
    let spent = a.observed_height.is_some()
        && a.inputs.iter().all(|id| {
            notes
                .iter()
                .any(|n| n.id == *id && n.spent_height == a.observed_height)
        });
    let change = a.prepared.as_ref().is_some_and(|p| {
        notes
            .iter()
            .any(|n| n.internal && n.id.txid == p.txid && Some(n.mined_height) == a.observed_height)
    });
    let settled = store
        .http_payment_settled(id)
        .await
        .map_err(|_| unavailable())?;
    let stage = if settled {
        "paid"
    } else if confirmations >= 3 && spent && change {
        "confirmed_observed"
    } else if confirmations > 0 {
        "awaiting_confirmations"
    } else if a
        .prepared
        .as_ref()
        .is_some_and(|p| height >= p.expiry_height)
    {
        "blocked_historical_absence_unproven"
    } else if a.broadcast {
        "broadcast"
    } else if a.prepared.is_some() {
        "payout_prepared"
    } else {
        "reserved"
    };
    Ok(Some(
        json!({"status":stage,"confirmations":confirmations,"required_confirmations":3,"txid":a.prepared.as_ref().map(|p|txid_text(p.txid)),"inputs_spent":spent,"internal_change_observed":change,"broadcast_acknowledged":a.broadcast,"expiry_height":a.prepared.as_ref().map(|p|p.expiry_height)}),
    ))
}
async fn redeem(State(s): State<Arc<BridgeApi>>, Path(id): Path<String>) -> Reply {
    let id = redeem_id(&id)?;
    let mut store = s.store.lock().await;
    let tip = store
        .zama_cursor()
        .await
        .map_err(|_| unavailable())?
        .map_or(0, |c| c.tip.number);
    let events = store.zama_events().await.map_err(|_| unavailable())?;
    let request = events
        .iter()
        .find(|e| matches!(e.event,Event::RedeemRequested{redeem_id,..} if redeem_id==id));
    let record = store
        .bridge_redeems()
        .await
        .map_err(|_| unavailable())?
        .into_iter()
        .find(|r| r.id == id && r.canonical);
    if record.is_none() && request.is_none() {
        return Err(missing());
    }
    let confirmations = request
        .and_then(|e| tip.checked_sub(e.block.number))
        .map_or(0, |n| n + 1);
    let payment = if let Some(r) = &record {
        payment(&mut store, &r.operation_id).await?
    } else {
        None
    };
    let stage = match record.as_ref().map(|r| r.state) {
        Some(RedeemState::Zero) => "completed_zero",
        Some(RedeemState::Rejected) => "rejected_before_payout",
        Some(RedeemState::Paid) => "paid",
        Some(RedeemState::Fulfilled) => "fulfilled",
        Some(RedeemState::RecreditPending) => "recredit_pending",
        Some(RedeemState::Recredited) => "re_credited",
        Some(RedeemState::Reserved) => payment
            .as_ref()
            .and_then(|p| p["status"].as_str())
            .unwrap_or("reserved"),
        _ if confirmations < 2 => "awaiting_confirmations",
        _ => "requested",
    };
    let fulfillment = events
        .iter()
        .find(|e| matches!(e.event,Event::RedeemFulfilled{redeem_id,..} if redeem_id==id));
    let recredit = events
        .iter()
        .find(|e| matches!(e.event, Event::RedeemRecredited { redeem_id, .. } if redeem_id == id));
    Ok(Json(
        json!({"id":id.to_string(),"operation_id":record.as_ref().map(|r| &r.operation_id),"recredit":recredit.map(|e|json!({"transaction":e.transaction_hash,"confirmations":tip.checked_sub(e.block.number).map_or(0,|n|n+1),"settled":record.as_ref().is_some_and(|r|r.state==RedeemState::Recredited)})),"status":stage,"version":record.as_ref().map(|r|r.version.to_string()),"burn_confirmations":confirmations,"required_evm_confirmations":2,"payment":payment,"fulfillment":fulfillment.map(|e|json!({"transaction":e.transaction_hash,"confirmations":tip.checked_sub(e.block.number).map_or(0,|n|n+1),"settled":record.as_ref().is_some_and(|r|r.state==RedeemState::Fulfilled)}))}),
    ))
}
async fn payout(State(s): State<Arc<BridgeApi>>, Path(id): Path<String>) -> Reply {
    if id.len() > 128 {
        return Err(invalid());
    }
    Ok(Json(
        payment(&mut *s.store.lock().await, &id)
            .await?
            .ok_or_else(missing)?,
    ))
}
async fn reconcile(State(s): State<Arc<BridgeApi>>) -> Reply {
    let store = s.store.lock().await;
    let report = store.http_reconcile().await.map_err(|_| unavailable())?;
    let health = store.http_health().await.map_err(|_| unavailable())?;
    Ok(Json(json!({"report":report,"persisted":health})))
}
async fn status(State(s): State<Arc<BridgeApi>>) -> Reply {
    let mut store = s.store.lock().await;
    let health = store.http_health().await.map_err(|_| unavailable())?;
    let redeems = store.bridge_redeems().await.map_err(|_| unavailable())?;
    let deposits = store.bridge_deposits().await.map_err(|_| unavailable())?;
    let recovery = recovery_progress(&mut store).await?;
    let mut refunds = Vec::new();
    for f in store.bridge_refunds().await.map_err(|_| unavailable())? {
        let void = store
            .zama_transaction(&f.void_operation_id)
            .await
            .map_err(|_| unavailable())?
            .map(|t| t.signed.hash());
        refunds.push(json!({"operation_id":f.operation_id,"void_transaction":void,"deposit_id":f.deposit_id,"state":format!("{:?}",f.state),"version":f.version.to_string(),"payment":payment(&mut store,&f.operation_id).await?}));
    }
    let ready = s.ready.load(Ordering::Acquire)
        && health["halted"] == false
        && health["report_valid"] == true;
    Ok(Json(
        json!({"persisted":health,"coordinator_ready":ready,"recovery":recovery,"deposits":deposits.iter().map(|d|json!({"operation_id":d.operation_id,"deposit_id":d.deposit_id,"state":format!("{:?}",d.state),"version":d.version.to_string(),"canonical":d.canonical})).collect::<Vec<_>>(),"redeems":redeems.iter().map(|r|json!({"operation_id":r.operation_id,"id":r.id.to_string(),"state":format!("{:?}",r.state),"version":r.version.to_string(),"canonical":r.canonical})).collect::<Vec<_>>(),"refunds":refunds}),
    ))
}
async fn recovery_progress(store: &mut Store) -> Result<Vec<Value>, ApiError> {
    Ok(store.payout_histories().await.map_err(|_| unavailable())?.into_iter().map(|d| {
        json!({"redeem_id":d.redeem_id.to_string(),"status":d.status,"acted_on":d.acted_on,
            "cursor_height":d.history.as_ref().map(|h|h.cursor_height()),
            "checkpoint_height":d.history.as_ref().map(|h|h.checkpoint_height()),
            "complete_absence":d.status == "complete" && d.history.as_ref().is_some_and(|h|h.complete_absence()),"persisted_only":true,"fresh_revalidation_required":true})
    }).collect())
}
async fn recovery(State(s): State<Arc<BridgeApi>>) -> Reply {
    let mut store = s.store.lock().await;
    let progress = recovery_progress(&mut store).await?;
    let health = store.http_health().await.map_err(|_| unavailable())?;
    let redeems = store.bridge_redeems().await.map_err(|_| unavailable())?;
    let complete = health["halted"] == false
        && progress.iter().any(|p| {
            p["complete_absence"] == true
                && redeems.iter().any(|r| {
                    r.id.to_string() == p["redeem_id"].as_str().unwrap_or("")
                        && r.canonical
                        && matches!(
                            r.state,
                            RedeemState::Reserved | RedeemState::RecreditPending
                        )
                })
        });
    Ok(Json(
        json!({"enabled":complete,"reason":if complete {"explicit_recredit_authorization_required"} else {"blocked_historical_absence_unproven"},"requests":progress}),
    ))
}
async fn request_recovery(
    State(s): State<Arc<BridgeApi>>,
    v: Result<Json<Recredit>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let p = body(v)?;
    let version = decimal_u64(&p.version)?;
    let id = redeem_id(&p.redeem_id)?;
    tokio::spawn(async move {
        let _payment = s.payment_lock.lock().await;
        let mut store = s.store.lock().await;
        let pair = checkpoints(&s, &mut store).await?;
        let d = store.request_payout_history(id, version, pair).await.map_err(|_| conflict())?;
        Ok((StatusCode::ACCEPTED, Json(json!({"redeem_id":d.redeem_id.to_string(),"status":d.status,"authorization_required":true}))))
    }).await.map_err(|_| unavailable())?
}
async fn disabled_payout() -> Reply {
    Err(ApiError(
        StatusCode::CONFLICT,
        "arbitrary_payout_disabled_in_bridge_domain",
    ))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Refund {
    deposit_id: B256,
    version: String,
    participant: String,
    destination: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Recredit {
    redeem_id: String,
    version: String,
}
// Fresh external canonical checks precede the store's locked checkpoint recheck.
async fn checkpoints(s: &BridgeApi, store: &mut Store) -> Result<BridgeCheckpointPair, ApiError> {
    let pair = store
        .bridge_checkpoint_pair()
        .await
        .map_err(|_| conflict())?;
    s.contract
        .check_canonical(pair.checkpoint)
        .await
        .map_err(|_| unavailable())?;
    let latest = zama::watcher::Watcher::new(&s.contract)
        .latest()
        .await
        .map_err(|_| unavailable())?;
    if latest.reference.number <= pair.checkpoint.number {
        return Err(conflict());
    }
    let client = LightwalletClient::connect(&s.zcash_endpoint)
        .await
        .map_err(|_| unavailable())?;
    if u64::from(client.tip().await.map_err(|_| unavailable())?) < u64::from(pair.zcash_height) + 2
        || client
            .block_hash(pair.zcash_height)
            .await
            .map_err(|_| unavailable())?
            != pair.zcash_hash
    {
        return Err(conflict());
    }
    Ok(pair)
}
async fn refund(
    State(s): State<Arc<BridgeApi>>,
    v: Result<Json<Refund>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let p = body(v)?;
    let version = decimal_u64(&p.version)?;
    tokio::spawn(async move {
        let mut store=s.store.lock().await;let pair=checkpoints(&s,&mut store).await?;
        let f=store.authorize_bridge_refund_at(p.deposit_id,version,&p.participant,&p.destination,pair).await.map_err(|_|conflict())?;
        Ok((StatusCode::ACCEPTED,Json(json!({"operation_id":f.operation_id,"deposit_id":f.deposit_id,"state":format!("{:?}",f.state),"version":f.version.to_string()}))))
    }).await.map_err(|_|unavailable())?
}
async fn recredit(
    State(s): State<Arc<BridgeApi>>,
    v: Result<Json<Recredit>, JsonRejection>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let p = body(v)?;
    let version = decimal_u64(&p.version)?;
    let id = redeem_id(&p.redeem_id)?;
    tokio::spawn(async move {
        let _payment = s.payment_lock.lock().await;
        let mut store=s.store.lock().await;let pair=checkpoints(&s,&mut store).await?;
        let client = LightwalletClient::connect(&s.zcash_endpoint).await.map_err(|_|unavailable())?;
        let r=crate::core::payout_death::authorize_recredit(&mut store,&client,id,version,pair).await.map_err(|_|conflict())?;
        Ok((StatusCode::ACCEPTED,Json(json!({"operation_id":r.recredit_id(),"id":r.id.to_string(),"state":format!("{:?}",r.state),"version":r.version.to_string()}))))
    }).await.map_err(|_|unavailable())?
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
