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
