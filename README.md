# cWZEC / MAZE bridge

A trusted-operator testnet bridge between shielded Zcash (Ironwood) and confidential
cWZEC on Sepolia. The backend and web frontend are implemented.

- **Deposit:** send shielded testnet ZEC to the vault with a recipient memo; the
  bridge mints the matching encrypted cWZEC amount after confirmation and verifies
  the actual result.
- **Redeem:** authorize a cWZEC burn in MetaMask; the bridge verifies the actual
  burn and sends shielded ZEC to the supplied destination, minus the redemption fee.
- **Reveal:** authorize confidential balance decryption in the browser. Signing
  keys remain in the user's wallet.

The operator controls vault custody and mint/payment authorization, and can read
bridge amounts and destinations. This is not a trustless or mainnet deployment.
The policy is three Zcash confirmations and two canonical EVM confirmations;
these are reorgable. Verification scope and limitations are recorded separately.

## Run the existing deployment

Use Rust 1.92.0, Node 24.22.1 and PostgreSQL. From the repository root, with the
existing `.local/` configuration and matching PostgreSQL database preserved:

```sh
export RUSTC="$(rustup which --toolchain 1.92.0 rustc)"
export PATH="$(dirname "$RUSTC"):$PATH"
cargo build --manifest-path service/Cargo.toml -p app --bin bridge-auto --release --locked --offline
```

Start each process in its own terminal:

```sh
# Internal FHE sidecar, port 8788. NODE_BIN may select a local Node installation.
SIDECAR_PORT=8788 ./scripts/sidecar-start.sh
```

```sh
# Coordinator and HTTP API, port 8787.
./scripts/run-local.sh
```

```sh
# Frontend, normally http://localhost:5173.
npm --prefix web run dev
```

Use MetaMask on Sepolia and a shielded Zcash testnet wallet such as Zingo. The web
app obtains the active vault address, contract, chain and fees from `/v1/info`.
Do not run a second coordinator against the same deployment. For installation,
configuration, shutdown and recovery, see [operations](docs/operations.md).

## Use the bridge

1. Connect your destination EVM wallet, copy the deposit address and complete memo
   from the Deposit view, and send shielded testnet ZEC from your Zcash wallet.
2. Paste the Zcash transaction ID into the tracker. `minted` means the actual
   encrypted mint has been reconciled; reaching two EVM confirmations alone does
   not imply this step is finished.
3. Use Reveal to authorize private balance decryption.
4. On Redeem, enter the amount and an explicit shielded testnet destination, then
   authorize the transaction in MetaMask. Tracking follows the redemption ID.

The redemption fee is currently 10,000 zatoshis (0.0001 ZEC), with a minimum gross
redemption of 10,001 zatoshis. Use the API's values rather than hardcoding them.
Redemption payouts can combine up to 16 confirmed notes. The network fee is
5,000 zatoshis per action (minimum two); any excess above the quoted user fee
comes from the operator reserve, which must retain a 10,000-zatoshi buffer.
Pending payouts reserve that extra fee capacity too. Refunds still use one input.
All amounts have eight decimals. Do not resend a deposit or burn to resolve a
slow status update or an uncertain submission.

For a wallet-only deposit using curl:

```sh
curl --noproxy '*' -sS http://127.0.0.1:8787/v1/info
curl --noproxy '*' -sS 'http://127.0.0.1:8787/v1/memo?recipient=0xYOUR_EVM_ADDRESS'
curl --noproxy '*' -sS http://127.0.0.1:8787/v1/deposits/YOUR_ZCASH_TXID
```

The memo endpoint returns only the memo; `/v1/info` supplies the unified deposit
address. Public tracking needs no wallet signature or operator API key.

## Repository

| Path | Purpose |
|---|---|
| `service/crates/app` | HTTP API, workflows, PostgreSQL state and journals |
| `service/crates/zcash` | Shielded scanning, vault operations, signing and transactions |
| `service/crates/zama` | Contract client, canonical observations and FHE sidecar client |
| `contracts` | Confidential token and contract tests |
| `sidecar` | Internal Node FHE SDK service; no signing keys |
| `web` | React frontend, MetaMask integration and browser balance reveal |
| `scripts` | Startup, operator commands and acceptance tools |
| `probes` | Retained feasibility tooling, not the production entry point |

## Documentation

README files are versioned. The additional design, operations and verification
records below are retained locally and excluded from Git.

- [Architecture and trust model](cwzec-bridge-design.md)
- [Installation, operation and recovery](docs/operations.md)
- [HTTP API](docs/api.md)
- [Frontend setup and implementation](web/README.md)
- [UI design reference](cwzec-ui-design.md)
- [Internal sidecar API](sidecar/API.md)
- [Verification record and limitations](docs/backend-acceptance.md)
- [Issued-payout recovery evidence](docs/payout-death-evidence.md)
- [Historical implementation plan and agent handoffs](docs/archive/README.md)

## Checks

```sh
npm --prefix web test
npm --prefix web run build
npm --prefix sidecar run check
npm --prefix sidecar test
./scripts/contracts-test.sh
```

Rust integration tests require a **disposable** PostgreSQL database. Never point
them at the funded bridge database. See [verification](docs/backend-acceptance.md)
for the recorded acceptance and reproduction commands. Test code stays in separate
files. This documentation cleanup does not rerun transfers or acceptance.
