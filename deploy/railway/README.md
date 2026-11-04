# Railway demo hosting

Three services: `bridge` (Rust coordinator + loopback FHE sidecar), `Postgres`
(persistent database), and `prices` (standalone signer). Use one bridge replica,
no sleeping/serverless mode, and no automatic bridge deployments during migration.

Build from the repository root:

| Service | `RAILWAY_DOCKERFILE_PATH` | Public route |
|---|---|---|
| bridge | `deploy/railway/bridge.Dockerfile` | `/v1/*` |
| prices | `deploy/railway/price.Dockerfile` | `/quote` |

The bridge starts in standby unless `BRIDGE_ACTIVATE=1`. Standby has no HTTP
listener and performs no scanning/signing. Do not enable an HTTP healthcheck
until activation. Once active, `/v1/info` checks HTTP availability; its JSON
`scanner_ready` separately indicates bridge readiness.

## Configuration

Use Railway variables, never Docker build arguments or committed files.

Bridge:
- `DATABASE_URL`: reference Postgres's **private-network** URL.
- `ZAMA_RPC_URL`, `ZAMA_CHAIN_ID=11155111`, `ZAMA_CONTRACT`, `ZAMA_DEPLOYMENT_BLOCK`.
- `ZCASH_LIGHTWALLETD_URL`, `CONFIRMATIONS=3`, `ADMIN_API_KEY`.
- `VAULT_KEY_BASE64`, `BRIDGE_SERVICE_KEY_BASE64`, `BRIDGE_AUDITOR_KEY_BASE64`,
  `BRIDGE_DESTINATION_KEY_BASE64`: base64 of each original 32-byte key file.
- `BRIDGE_ACTIVATE=0` until database restore and local shutdown are verified.

The launcher writes private temporary key files, sets their existing `*_KEY_PATH`
variables, and starts the sidecar with only its RPC and loopback-port settings.
The sidecar receives no signing keys. `PORT` maps to `BRIDGE_HTTP_PORT`;
`BRIDGE_HTTP_HOST=0.0.0.0` is set by the image. Normal local startup still defaults
to `127.0.0.1`. No bridge filesystem volume is needed: durable state is PostgreSQL;
original keys must remain backed up separately.

Prices: `PRICE_SIGNING_KEY`, `SWAP_ADDRESS`. The image binds `0.0.0.0:$PORT`.
Never give this service the vault, service, auditor or destination keys.

## Cutover

1. Build/deploy bridge in standby and prices separately; check trial outbound
   connectivity and memory limits. Attach persistent storage to PostgreSQL.
2. Stop the local coordinator and wait for drain completion. Prevent it restarting.
   A database lease cannot protect two independent copies of a database.
3. Make a private PostgreSQL 17 custom-format backup of the original database.
   Restore into the empty hosted database with `pg_restore --no-owner --no-acl`.
   Keep the original database and key files untouched. Never reset scan heights.
4. Compare domain, checkpoint, note, transaction, reservation and liability state.
   Check PostgreSQL disk headroom including WAL, not just logical database size.
5. Set matching key/RPC variables and activate exactly one hosted coordinator.
   Check `/v1/info`, readiness, known deposit/redemption status and restart recovery.
6. Set Netlify `VITE_API_BASE` to bridge HTTPS origin and `VITE_SWAP_QUOTE_URL` to
   prices HTTPS origin, then rebuild. Verify Nym ingress and user-approved wallet
   flows. Never count read-only HTTP checks as a completed transfer test.

For bridge updates, stop/drain the old deployment before starting a replacement;
rolling overlap can conflict with the coordinator lease. Set a suitable teardown
grace (e.g. 300 seconds). Forced termination retains journals but still requires
reconciliation on restart.

After hosted actions occur, the local database is stale. Rollback requires stopping
the hosted coordinator and restoring its latest state locally before starting it.
Do not simply restart the old local copy. Cutover creates the local marker
`.local/hosted-bridge-active`, which blocks `scripts/run-local.sh`; remove it only
after the reverse migration is complete. Direct binary invocation bypasses this
guard. Restore to a PostgreSQL version at least as new as the hosted server.

Trial credits expire; this setup is not a promise of indefinitely free hosting.
Check real runtime/proving memory and storage growth before relying on it.

## Deployment — 2026-11-04

- Project: `cwzec-demo` (`dec4e252-d8da-4939-b90c-889dc564f932`).
- Bridge: `https://bridge-production-8771.up.railway.app`.
- Prices: `https://prices-production-e006.up.railway.app`.
- PostgreSQL 18 restored from PostgreSQL 17; all 22 public tables matched by
  counts and UTC-normalized row digests before activation.
- Trial volume usage after restore: approximately 360 / 500 MB, including WAL.
  `max_wal_size=64MB`, `min_wal_size=32MB`; monitor growth. These are checkpoint
  targets, not an absolute WAL cap. Verification sorts hashes locally to avoid
  exhausting the small database volume with temporary files.
- HTTP info/readiness, memo checksum, known minted deposit, fulfilled redeem,
  anonymous admin denial and clean restart/reconciliation passed.
- Hosted price signature and deployed swap `rate()` read-only verification passed.
- No new transfer, funded swap or wallet decryption acceptance was performed.
- Migration backup, deployment metadata and verification evidence are in ignored
  `.local/railway-*` files. Original key files and local database remain preserved.

Set these Netlify build variables and redeploy:

```env
VITE_API_BASE=https://bridge-production-8771.up.railway.app
VITE_SWAP_QUOTE_URL=https://prices-production-e006.up.railway.app
VITE_BRIDGE_MODE=live
NODE_VERSION=24
```

After the frontend deploy, check Deposit and Swap through Nym against these
origins. Retire local ngrok/Cloudflare tunnels and the local price/sidecar processes
only after the frontend has switched. Railway now runs the backend independently
of the Mac. Frontend-to-Railway Nym ingress verification remains pending.
