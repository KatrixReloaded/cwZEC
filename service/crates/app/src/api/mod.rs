use crate::core::{PayoutRequest, Service, deposit_id, make_memo, parse_memo};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    routing::{get, post},
};
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    sync::{Arc, atomic::Ordering},
    time::Duration,
};
use subtle::ConstantTimeEq;

pub fn router(service: Arc<Service>) -> Router {
    Router::new()
        .route("/v1/info", get(info))
        .route("/v1/memo", get(memo))
        .route("/v1/deposits", get(deposits))
        .route("/v1/admin/payout", post(payout))
        .route("/v1/payouts/{id}", get(payout_status))
        .with_state(service)
}

async fn info(State(s): State<Arc<Service>>) -> Result<Json<Value>, StatusCode> {
    let state = s
        .store
        .lock()
        .await
        .load()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(
        json!({"network":"testnet", "pool":"ironwood", "attested":false,
        "deposit_address":s.deposit_address, "confirmations":s.confirmations,
        "scan_height":state.checkpoint.height, "scanner_ready":s.healthy.load(Ordering::Acquire),
        "memo_version":"cwzec:v1", "chain_id":11155111,
        "fixed_fee_zat":zcash::vault::FIXED_FEE_ZAT}),
    ))
}

#[derive(Deserialize)]
struct MemoRequest {
    recipient: String,
}

async fn memo(Query(request): Query<MemoRequest>) -> Result<Json<Value>, StatusCode> {
    Ok(Json(
        json!({"memo":make_memo(&request.recipient).map_err(|_| StatusCode::BAD_REQUEST)?}),
    ))
}

fn authorize(s: &Service, headers: &HeaderMap) -> Result<(), StatusCode> {
    authorize_key(&s.admin_key, headers)
}

fn authorize_key(expected: &str, headers: &HeaderMap) -> Result<(), StatusCode> {
    let supplied = headers
        .get("x-api-key")
        .and_then(|v| v.to_str().ok())
        .ok_or(StatusCode::UNAUTHORIZED)?;
    if supplied.as_bytes().ct_eq(expected.as_bytes()).into() {
        Ok(())
    } else {
        Err(StatusCode::UNAUTHORIZED)
    }
}

async fn deposits(
    State(s): State<Arc<Service>>,
    headers: HeaderMap,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let mut store = s.store.lock().await;
    let height = store
        .load()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .checkpoint
        .height;
    let records = store
        .deposits()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let records: Vec<_> = records
        .iter()
        .map(|n| {
            let confirmations = height.saturating_sub(n.mined_height) + 1;
            let recipient = parse_memo(&n.memo).ok();
            json!({"txid":txid_text(n.id.txid), "action_index":n.id.action_index,
            "deposit_id":hex::encode(deposit_id(&n.rho, &n.rseed)),
            "recipient":recipient.map(|r|format!("0x{}",hex::encode(r))),
            "kind":if n.internal {"internal"} else {"external"},
            "value_zat":n.value,
            "memo_valid":recipient.is_some(), "confirmations":confirmations,
            "confirmed":confirmations >= s.confirmations, "spent":n.spent_height.is_some()})
        })
        .collect();
    Ok(Json(json!({"deposits":records})))
}

pub fn txid_text(mut bytes: [u8; 32]) -> String {
    bytes.reverse();
    hex::encode(bytes)
}

async fn payout(
    State(s): State<Arc<Service>>,
    headers: HeaderMap,
    Json(request): Json<PayoutRequest>,
) -> Result<(StatusCode, Json<Value>), StatusCode> {
    authorize(&s, &headers)?;
    let id = request.operation_id.clone();
    s.prepare_payout(request)
        .await
        .map_err(|_| StatusCode::CONFLICT)?;
    Ok((StatusCode::ACCEPTED, Json(json!({"operation_id":id}))))
}

async fn payout_status(
    State(s): State<Arc<Service>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<Json<Value>, StatusCode> {
    authorize(&s, &headers)?;
    let mut store = s.store.lock().await;
    let attempt = store
        .get_attempt(&id)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?
        .ok_or(StatusCode::NOT_FOUND)?;
    let state = store
        .load()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let confirmations = attempt
        .observed_height
        .map_or(0, |h| state.checkpoint.height.saturating_sub(h) + 1);
    let stage = if confirmations >= s.confirmations {
        "confirmed"
    } else if confirmations > 0 {
        "confirming"
    } else if attempt
        .prepared
        .as_ref()
        .is_some_and(|p| state.checkpoint.height >= p.expiry_height)
    {
        "blocked_expiry_unresolved"
    } else if attempt.broadcast {
        "broadcast"
    } else if attempt.prepared.is_some() {
        "prepared"
    } else {
        "reserved"
    };
    let records = store
        .deposits()
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    let change = attempt
        .prepared
        .as_ref()
        .is_some_and(|p| records.iter().any(|n| n.internal && n.id.txid == p.txid));
    let inputs_spent = attempt.observed_height.is_some()
        && attempt.inputs.iter().all(|id| {
            records
                .iter()
                .any(|n| n.id == *id && n.spent_height == attempt.observed_height)
        });
    Ok(Json(
        json!({"operation_id":id,"status":stage,"confirmations":confirmations,
        "txid":attempt.prepared.as_ref().map(|p|txid_text(p.txid)),
        "signed_bytes_digest":attempt.prepared.as_ref().map(|p|hex::encode(Sha256::digest(&p.bytes))),
        "inputs_spent":inputs_spent,"internal_change_observed":change}),
    ))
}

pub async fn scan_loop(service: Arc<Service>) {
    loop {
        if service.scan_once().await.is_err() {
            eprintln!("scan unavailable; payout preparation blocked");
        } else if service.recover().await.is_err() {
            eprintln!("payment recovery unresolved; stored attempts remain reserved");
        }
        tokio::time::sleep(Duration::from_secs(10)).await;
    }
}

mod bridge;
pub use bridge::{BridgeApi, bridge_router};
