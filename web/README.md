# MAZE / cWZEC bridge — web

React port of `../maze-bridge-design`. `src/styles/cw.css` is the design's stylesheet.

```sh
npm ci
VITE_BRIDGE_MODE=demo npm run dev   # simulated backend and wallet actions; no network requests, nothing signed
VITE_API_BASE=https://api.… npm run build && npm run preview   # live, through Nym
npm test && npm run build
```

| Variable | Default | Meaning |
|---|---|---|
| `VITE_BRIDGE_MODE` | live | `demo` uses `src/api/mock.ts` + `src/wallet/mockChain.ts`; the header pill reads "Demo" |
| `VITE_API_BASE` | unset | Public `https://` origin of the bridge API. Live requests are refused while it is unset |

## Nym routing

Every live bridge API request (info, memo, deposit/redeem status, redeem prepare) goes through
`src/api/nym.ts` → `@nymproject/mix-fetch` 3.0.1 (pinned; wraps `@nymproject/mix-tunnel` 0.3.1).
The API ingress sees a Nym exit IP. Netlify still sees the page visit; MetaMask, Zingo and the
Zama relayer are not routed. The API still receives the addresses and request data it needs.

- One tunnel per page, started lazily by the first request (an 18 MB lazy chunk with inlined WASM and
  worker), default mixing and cover traffic. All live requests wait for it.
- No native `fetch` fallback. If setup fails or a GET times out, the connection requires a page
  reload; the banner reads "Couldn't connect through Nym." with Reload. Other request failures
  use the existing "Bridge unavailable".
- `maxRedirects: 0`: a redirect with `Location` rejects (`too many redirects (>0)`). Other 3xx fail as
  HTTP errors. The SDK never shares the browser's cookies, and we send no credentials.
- mix-fetch can't cancel. Aborting (new key, unmount, newer memo request) drops the late result, but
  the request may still reach the API and keeps its slot until it settles. At most 6 are outstanding;
  more are rejected, not queued. Polls are scheduled only after the previous request settles. A redeem
  prepare that hits the 60 s UI timeout stays attached: Retry waits on that same POST and doesn't send another.
- Local development: no `/v1` proxy (a remote exit can't reach localhost). Use demo mode, or point
  `VITE_API_BASE` at a public HTTPS API.

### Netlify

`netlify.toml`: base directory `web`, build `npm ci && npm run build`, publish `dist`, plus the CSP:

- `connect-src 'self' https://validator.nymtech.net wss: https://relayer.testnet.zama.org` is the Nym
  bootstrap, the gateway WebSockets (hosts and ports such as `:9001` come from topology, so `wss:`
  must stay broad), and the Zama relayer for balance reveal. The API origin is deliberately absent, so
  any direct browser request to it is blocked.
- `worker-src 'self' blob:` and `script-src 'self' 'wasm-unsafe-eval'` cover the tunnel worker and the WASM.
- Fonts come from Google Fonts. There is no API proxy or function.

### Remaining once the API URL exists

1. Set `VITE_API_BASE=https://api.…` (no path or trailing slash) in Netlify's build environment and redeploy.
2. Make sure that origin isn't listed in `connect-src`. Keep it off `'self'`, i.e. not the Netlify site's own origin.
3. Make sure the API answers `/v1/*` directly, with no 3xx, over a publicly trusted certificate, and
   that Nym exit policies allow its host and port (443).
4. From the deployed site, load info, a memo and a status lookup. Confirm in the **API ingress logs**
   that the source IPs are Nym exits, not the test client's IP. Behind a reverse proxy, check the
   proxy's logs; the Rust process may only see the proxy.
5. Block `validator.nymtech.net` (or go offline) and reload. Expect the Nym banner, a CSP-blocked or no
   direct request to the API, and nothing in ingress.
6. Reveal a balance once with a real wallet, to confirm the CSP is enough for the Zama relayer.
7. Measure tunnel startup (about 20 s here) and request latency. Only revisit `POLL_MS` if needed.

### Verified 2026-11-04 (no bridge API yet)

The production build was served with the `netlify.toml` CSP in headless Chromium. `VITE_API_BASE` pointed at a public test origin, with no project data sent.
- The tunnel was ready in about 20 s. Bootstrap went only to `validator.nymtech.net`, then one `wss://…:9001` gateway.
- The exit IP (ipify, and httpbin's `origin` on a POST) differed from the direct IP.
- A redirect was rejected, and a direct `fetch` was blocked by CSP. That was the only CSP violation.
- The demo build made no requests except assets and fonts, and started no worker.

Live bridge acceptance (steps 4–6 above) is still pending.

### Known SDK limitations

- No `AbortSignal`, `credentials`, `redirect` or per-request timeout. The SDK serialises requests per origin.
- One-shot tunnel: after a failure or `disconnectMixTunnel()`, only a page reload recovers.
- Large bundle (18 MB, 6.4 MB gzip) and roughly 20 s startup before the first API response.
- It injects browser-like default headers (`User-Agent`, `Accept-Language`, `Accept-Encoding: identity`).
- Exit IPs vary per gateway/IPR; APIs that rate-limit by IP see shared exits.

No RPC URL or key is configured: chain reads, transactions and permits go through the user's
MetaMask. The bridge's chain, contract, deposit address, fee and minimum come from `/v1/info`.

## What the live UI does

- **Transport** — all of the below go through Nym (see above).
- **Status** — `GET /v1/info`, `/v1/deposits/{txid}`, `/v1/redeems/{id}`, polled 5 s after each
  attempt settles; stale data is labelled. GET calls have a 60 s UI deadline; a timeout marks
  the tunnel stalled and shows Reload instead of adding retries to that connection. Requests
  cancelled during setup are not dispatched. Calls already dispatched retain their Nym slot
  until the SDK settles them; the six-request bound still applies. Mint/payout completion
  follows backend status only.
- **Memo** — `GET /v1/memo?recipient=…`, checked to belong to that recipient; a newer recipient
  aborts the older request.
- **Redeem** — one `operation_id` per intent; `POST /v1/redeem/prepare` (retries reuse ID and terms);
  the response is verified before MetaMask sees it (operation, from, to, chain, zero value, exact
  `requestRedeem(bytes32,bytes,bytes)` calldata, euint64 input handle for this chain). One
  `eth_sendTransaction` per intent; rejection, wallet change and ambiguous outcomes are distinct and
  never re-sent automatically. The receipt's single `RedeemRequested` from this contract and user
  yields the redemption ID for the tracker. Starting new work requires a ready, unhalted bridge.
  Only `{opId, hash, redeemId}` is kept in `localStorage`, keyed by chain/contract/account.
- **Balance** — `@zama-fhe/sdk` 3.7.1 in the browser (lazy chunk): read the handle, zero handle → 0
  without signing, otherwise one EIP-712 user-decryption permit in MetaMask and relayer decryption.
  Fresh SDK per reveal with in-memory storage, terminated afterwards; plaintext only in React state,
  cleared on hide, account/network change and disconnect.

The encrypted amount and destination in the prepared calldata cannot be checked in the browser;
they are bound to the user and contract by the backend's encryption.

## Layout

| Path | Role |
|---|---|
| `src/api/` | Bridge HTTP client (`live.ts`, `http.ts`), Nym transport (`nym.ts`), demo backend (`mock.ts`), types |
| `src/wallet/` | MetaMask (`metamask.ts`), send/receipt/reveal (`chain.ts`), browser FHE (`fhe.ts`) |
| `src/lib/redeemTx.ts` | Prepared-transaction verification and receipt/event decoding |
| `src/lib/redeemFlow.ts` | Redemption state machine (prepare → verify → send → receipt) |
| `src/lib/{balanceReveal,intentStore,latest,status,amount}.ts` | Reveal lifecycle, recovery storage, latest-request, status mapping, bigint amounts |
| `src/hooks/` | React glue (`usePoll`, `useBridgeInfo`, `useDeposit`, `useRedeem`, `useBalance`, `useWallet`) |
| `src/components/`, `src/views/` | UI |
| `src/**/*.test.ts`, `src/test/` | Vitest tests and fixtures |

## Swap · cwZEC → mock-backed cUSDC

`#swap` is a live-only Sepolia page, independent of bridge API availability. Demo
mode disables Swap rather than simulating a funded result. Existing Deposit and
Redeem logic is unchanged. New logic, styles and separate unit tests live in
`src/swap/`, with `src/views/SwapView.tsx` as the page.

| Variable | Default | Meaning |
|---|---|---|
| `VITE_SWAP_QUOTE_URL` | unset | Public HTTPS **origin** of the standalone price service, e.g. `https://prices.example.com`; the page requests `/quote` through the existing Nym tunnel |

The current service at `127.0.0.1:8789` is **not reachable by a Nym exit**. An
operator must arrange public HTTPS with a trusted certificate and no redirects,
then set `VITE_SWAP_QUOTE_URL` in the frontend build environment and rebuild.
Do not use a localhost URL, frontend same-origin proxy, credentials or secret API
keys. Keep the quote origin **absent** from CSP `connect-src`; the existing Nym
and bridge transport/CSP configuration needs no changes. This implementation
neither exposes nor restarts the service. Until setup is supplied, the page
shows **Price unavailable** and cannot submit a swap. MetaMask RPC, encryption
relayer traffic and wallet-authorized decryption use the existing wallet/Zama
integration, not Nym. Quotes contain no user address or amount.

On connection and before transactions, the page checks chain 11155111, deployed
code, token decimals/backing, balancer token and pool configuration through
MetaMask. Read-only live calls on 2026-11-04 verified:

- Pool `0xa47a8e065836082061F58D2a2c751f72F1226426`.
- cwZEC `0x91cac58cea711bd9487721824ea38e2b5742253f`.
- cUSDC `0xd2BBe2681a3cB05C73901dcc836f676801D013A3`, backed by mock USDC
  `0x12445A9dd878282004E1E76d6124A5D191FDbecB`.
- Balancer `0x89f2D06A3cD63b6c0a2eC14c341b3F8b02cE963c`.
- Quote signer `0xf988419246ab423a3C78075bF1dafB6BE087C6F8` and `feeBps() = 100`.

The displayed fee is read from the contract. Quotes are shape/range/freshness
checked and EIP-712 recovered for the exact CWZECSwap v1 domain. Requests are
serialized, polls start after prior work settles, and late results from previous
wallet scopes are ignored. Authorization is an explicit ERC7984 `setOperator`
for 10 minutes, allowing **any cwZEC amount** during that window; it is not an
ERC20 allowance. A new quote is fetched after its confirmation and at Swap.
The minimum displayed when Swap is clicked is retained even if a later price
falls. All amount, fee and slippage calculations use BigInt; no financial float
conversion is used. The default slippage is 0.5%.

SDK 3.7.1's browser `sdk.encrypt({values:[{type:'euint64',value:amount},
{type:'euint64',value:minimum}],contractAddress:pool,userAddress:seller})` binds
both inputs together. The legacy `createEncryptedInput` API in older notes is
not used. Expiry is checked again immediately before requesting a zero-ETH
transaction. Expiry while the wallet prompt is open can still cause a revert.
No failed or ambiguous transaction is automatically resubmitted.

The tracker requires two confirmations and checks the receipt block against the
canonical block. Repricing follows the replacement hash; cancellation or changed
calldata is reported as replacement, not a fill. Only `{kind, nonce, hash?}` is
saved, keyed by chain/pool/account, before the wallet send. Reload and choose
**Check confirmations** to resume; if the wallet returned no hash, enter it from
MetaMask activity. A recovery hash must have the expected sender and nonce.
An unresolved send blocks another send. A send with no obtainable hash stays
uncertain: inspect wallet activity instead of starting another transaction.

**Receipt success does not mean filled.** The pinned ERC7984
`ConfidentialTransfer` indexes all three values, including the payment handle.
Its `_update` grants both recipient and token access to the *transferred* handle.
The page selects exactly one cUSDC payment from this pool to this seller in the
confirmed receipt. **Verify exact payment · sign reveal** requests a cUSDC permit
and decrypts that specific handle through the installed SDK. Zero means
**Not filled — cwZEC refunded**; a positive value permits **Swapped** with the
exact cUSDC received. Missing/ambiguous events, a declined permit or a relayer
failure leave the result unverified: **Transaction confirmed — verify balances**.
Current balances are never used to infer a transaction's fill. Both tokens have
optional wallet-authorized balance reveal. Balances are hidden when transaction
tracking changes. Reveals, plaintext inputs, ciphertext payloads, permits and
transport keys are never persisted; scope changes/disconnect clear revealed
state and each SDK instance terminates with memory-only storage. No pool balance
is read or revealed, and available inventory is never assumed.

### Manual live acceptance (requires the user's wallet interaction)

1. Arrange the HTTPS quote origin above; build in live mode with
   `VITE_SWAP_QUOTE_URL=https://prices.example.com npm run build` from `web`.
   Serve the build with the existing Netlify CSP. Do not add its origin to CSP.
2. At desktop and mobile sizes, navigate Deposit → Swap → Redeem, including
   rapid clicks; verify the grid fade, active tabs, controls and no horizontal
   scroll. Confirm Deposit memo/status and Redeem prepare/tracking still work
   using their existing acceptance procedures and configured bridge API.
3. With HTTPS unset or unavailable, expect Price unavailable and disabled Swap.
   With it configured, verify `/quote` requests in service ingress logs come
   from Nym exits; browser requests should show no direct quote HTTP request.
   Block Nym bootstrap and reload: no native-fetch fallback may occur.
4. Connect MetaMask on another chain; verify the Sepolia switch. Connect the
   intended seller and check the fee is 1.0%. Reveal each private balance only
   by accepting its wallet permit. Switch account/network or disconnect while
   revealing: old plaintext/results must disappear.
5. Enter an amount with at most 8 decimals and select slippage. Verify estimate
   and minimum. If needed click **Authorize operator · 10 min**, inspect pool
   and expiry in MetaMask, and explicitly accept. First try rejection to confirm
   no swap follows. Authorization confirmation must fetch a fresh signed price.
6. Only when ready to spend testnet tokens/gas, click Swap yourself. Accept the
   zero-ETH swap in MetaMask before quote expiry. Repeated clicks must not create
   another send. During a separate attempt, delay beyond expiry, reject a wallet
   request, or change account/network; the page must stop or report the revert
   without resubmitting. The encrypted minimum must remain the clicked minimum.
7. Wait for two canonical confirmations. Inspect the explorer link, then click
   **Verify exact payment · sign reveal** and explicitly accept the permit.
   A positive transaction-specific amount should show Swapped with that amount.
   A zero payment should show Not filled — cwZEC refunded. If verification is
   unavailable/declined, retain the unverified confirmation and optionally reveal
   wallet balances. Do not treat a successful receipt or balance delta as a fill.
8. For a deliberately overlarge inventory request or minimum rejection in a
   user-approved test attempt, verify zero payment/refund despite receipt success.
   Do not assume the pool's initial funding remains available; do not fund it as
   part of this check. Gas is still spent on an encrypted rejection.
9. Reload during pending tracking, then Check confirmations. Test MetaMask speed
   up/cancel and an interrupted send separately. Verify replacement/cancellation
   reporting and no automatic resubmission. Confirm localStorage contains only
   public chain/pool/account-scoped tracking identifiers, never amounts/proofs or
   decryption material. If no hash is known, recover from wallet activity.

`npm test`, `node_modules/.bin/tsc --noEmit`, and `npm run build` from `web`
validate the frontend. `src/swap/*.test.ts` use synthetic keys, mocked wallets,
SDKs and receipts; they are **not live swap/decryption acceptance**. No funded
swap or live decryption has been observed during this implementation. Public
HTTPS setup and the user's MetaMask signatures remain required for live acceptance.

Local quote connectivity verified 2026-11-04: the user-supplied public HTTPS
origin is configured in ignored `web/.env.local` and included in the local
production build. A browser request through the shared Nym transport returned a
fresh quote, verified against the configured signer, with 46 seconds remaining;
no direct browser HTTP request to that origin was observed. All 119 frontend
tests and the production build passed. Hosted builds need the same environment
setting supplied separately. Wallet signatures, funded swaps and live decryption
acceptance remain pending.
