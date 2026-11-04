# MAZE · cWZEC

MAZE helps shielded ZEC users bridge to Ethereum while keeping token amounts and
balances confidential. Shielded ZEC is locked in a Zcash vault and represented
by confidential cWZEC using Zama FHE. Burning cWZEC releases shielded ZEC back to
the user's chosen wallet.

[Open the app](https://cwzec.netlify.app)

The demo runs on Zcash testnet (Ironwood) and Ethereum Sepolia. Nym routes frontend
API requests through an exit node, hiding the user's IP from the bridge API.
The trusted operator controls vault custody and can read bridge amounts and
destinations; Ethereum accounts and transaction activity remain public. Netlify,
wallet RPC and Zama relayer connections are outside the Nym API tunnel.

## User Flow

Use a shielded Zcash testnet wallet such as Zingo and MetaMask on Sepolia.

1. **Deposit:** connect MetaMask, copy the vault address and exact memo from
   Deposit, then send shielded testnet ZEC with that memo from your Zcash wallet.
2. **Track:** paste the Zcash transaction ID. Wait for **Minted**; confirmation
   counts alone do not mean mint reconciliation is complete.
3. **Reveal:** sign a decryption permit to view your confidential balance.
4. **Redeem:** enter a cWZEC amount and your shielded testnet destination, confirm
   the transaction in MetaMask, then track the shielded payout.

The app displays current fees and minimums. The bridge requires three Zcash and
two canonical Ethereum confirmations; these can still be reorganized. Do not
resubmit a deposit or redemption just because tracking is delayed.

The optional **Swap** flow exchanges cWZEC for mock-backed cUSDC using a signed
Kraken ZEC/USD quote, assuming USDC = $1. Authorize the swap contract, submit the
swap, then verify its private payment. A confirmed transaction can also mean a
full token refund. Live frontend swap acceptance is pending.

## API Endpoints

Public bridge API, served on port `8787` locally:

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/v1/info` | Vault address, chain, fees and readiness |
| GET | `/v1/memo?recipient=0x…` | Deposit memo for an Ethereum recipient |
| GET | `/v1/deposits/{txid}` | Deposit and mint status |
| POST | `/v1/redeem/prepare` | Prepare a redemption transaction for the user's wallet |
| GET | `/v1/redeems/{id}` | Burn, payout and recovery status |

Operator endpoints require `x-api-key`:

| Method | Endpoint | Purpose |
|---|---|---|
| GET | `/v1/deposits?recipient=0x…` | Deposits by recipient |
| GET | `/v1/payouts/{id}` | Payout status |
| GET | `/v1/admin/status` | Operations status |
| GET | `/v1/admin/reconcile` | Saved accounting report |
| GET / POST | `/v1/admin/recovery` | Inspect / request recovery evidence |
| POST | `/v1/admin/recredit` | Authorize eligible re-credit |
| POST | `/v1/admin/refund` | Authorize eligible deposit refund |

`POST /v1/admin/payout` is disabled for the backed bridge. The separate
[price service](price-service/README.md) exposes `GET /quote` for signed swap
prices. Operator and signing keys never belong in the frontend.

## Development

Rust 1.92, Node 24 and PostgreSQL. With deployment configuration, keys and database
provisioned, start each process in a separate terminal:

```sh
SIDECAR_PORT=8788 ./scripts/sidecar-start.sh
./scripts/run-local.sh
npm --prefix web run dev
```

See [frontend setup](web/README.md) for installation, public HTTPS API settings,
Netlify deployment and Nym routing. Runtime configuration and keys stay in ignored
`.local/` files. The startup commands require a built backend and configured
services; they do not provision a deployment.

## Future Plans

- Automatic cUSDC inventory balancing through ZEC redemption and USDC recycling.
- TEE compatibility for isolated signing and bridge execution.
- Distributed hosting for improved availability and resilience.
- Explore an official confidential pool with Zama if demand grows.
