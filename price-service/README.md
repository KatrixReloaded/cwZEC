# Kraken reference-price signer

Standalone Node/TypeScript service for Sepolia CWZECSwap. The pool explicitly
trusts this operator's dedicated signing key to report prices honestly. This is a
Kraken reference price, **not a guaranteed executable price**. There is no on-chain
proof of the Kraken response, independent aggregation, USDC depeg protection or
signer rotation. The pool assumes USDC=$1 and sells finite operator-funded inventory.

The fixed source is `https://api.kraken.com/0/public/Ticker?pair=ZECUSD`.
[Official format](https://docs.kraken.com/api-reference/market-data/get-ticker-information):
`error` must be empty; the default internal pair key is `XZECZUSD`, ask is `a[0]`
and bid is `b[0]`. A read-only request verified that format on 2026-11-04.
Both must be positive, bid <= ask, and each <= $1,000,000. Decimal strings are
parsed with BigInt; the midpoint is computed at full source precision, then
rounded down to eight USD decimals. Zero or priceE8 > 1e14 is rejected.

Fetches start immediately and repeat 15 seconds after the preceding refresh
finishes, with no overlap. HTTP headers and body share a 5-second timeout;
decoded responses are capped at 16 KiB and redirects are refused. The service
signs only a successful new observation. `observedAt` is the local successful
observation time, **not an exchange publication timestamp**. Quotes expire 60
seconds later. Failed refreshes preserve the previous quote only until its
original expiry. Clock rollback clears the cache and prevents signing until the
clock catches up; keep the host clock synchronized. Restart discards all cache
and clock state and requires a new source fetch. There is no fixed-price fallback.

## Setup and private configuration

Use Node 22 or 24, then from the repository root:

```sh
npm --prefix price-service ci
npm --prefix price-service run check
npm --prefix price-service run build
npm --prefix price-service test
```

Only ethers 6.16.0 is a runtime dependency, matching existing tooling. The local
lockfile overrides its unused WebSocket dependency to patched ws 8.21.0; this
service has no WebSocket, RPC, transaction submission, database or queue.

Provision a **dedicated secp256k1 key** explicitly through your private secret
management process. Never reuse bridge, vault, deployer or wallet keys. The
service does not generate keys, read keystores or source other services' files.
Store configuration outside Git, preferably outside this repository, with mode
0600. `.env.example` contains placeholders for the key and deployed pool.
Never put the key in command arguments, browser bundles or logs.

Required: `PRICE_SIGNING_KEY` (0x-prefixed 32-byte private key), `SWAP_ADDRESS`
(nonzero deployed pool). Fixed chain ID: 11155111. Optional: `BIND_HOST` (default
127.0.0.1), `PORT` (default 8787), `FRONTEND_ORIGIN` (one exact http(s) origin,
without a trailing slash). No wildcard CORS, credentials or generic proxy.

The selected constructor `QUOTE_SIGNER` is
`0xf988419246ab423a3C78075bF1dafB6BE087C6F8`. Confirm the dedicated private key
derives to this address **before deploying the pool**, using only
that key's private environment file (which need not yet contain SWAP_ADDRESS):

```sh
cd price-service
node --env-file=/absolute/private/price-signer.env --input-type=module -e \
  'import {Wallet} from "ethers"; console.log(new Wallet(process.env.PRICE_SIGNING_KEY).address)'
```

Deploy later with that signer address using the command in
[deployment instructions](../docs/cwzec-swap-implementation.md), then set
`SWAP_ADDRESS` to the resulting pool address in the private environment file.
No deployment or funding is performed by this service. Start explicitly:

```sh
# From repository root, after filling the private file:
node --env-file=/absolute/private/price-signer.env price-service/dist/service.js
# Or, if the dedicated variables are already exported:
npm --prefix price-service start
```

The server defaults to loopback. For Nym-enabled requests, the quote endpoint
will need public HTTPS reachability; terminate TLS in your chosen hosting setup.
Configure the exact frontend origin only when direct browser access is needed.

## GET /quote

200 JSON (integer fields are decimal strings):

```json
{
  "quote": {"priceE8":"5000000000","observedAt":"1793797500","expiresAt":"1793797560"},
  "signature":"0x...",
  "chainId":"11155111",
  "swapAddress":"0x..."
}
```

This is an illustrative shape. Before the first successful fetch and at/after
expiry: 503 `{"error":"Price unavailable"}`. All responses send
`Cache-Control: no-store`. Requests to other paths or methods return 404.
A quote is public and reusable by multiple sellers until expiry.

EIP-712 domain: name `CWZECSwap`, version `1`, chainId `11155111`,
verifyingContract equal to configured `SWAP_ADDRESS`. Ordered primary type:
`PriceQuote(uint64 priceE8,uint64 observedAt,uint64 expiresAt)`.

Later frontend integration: retrieve the quote, require returned chain/address
(and the recovered signer) to match the active pool, create encrypted amount and
minimum output bound to pool/seller, then include `quote` and `signature` in the
nonpayable swap call. Enforce the independent user deadline. Do not send ETH.
See the implementation document for exact ABI and confidential refund semantics.

## Validation

Unit tests in `test/service.test.mjs` use only a synthetic key and local HTTP
servers. `test/live-sample.mjs` is an explicit optional read-only source probe:

```sh
node price-service/test/live-sample.mjs
```

It fetches Kraken and locally signs/recovers a sample with a **synthetic test
key** and dummy pool; it never reads private configuration or signs transactions.
Never authorize that public test key on a deployed pool. Cross-language Solidity
verification and confidential swap regression commands are in the implementation
document. Local signatures and FHE mocks are not a funded on-chain exchange.
