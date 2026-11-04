/** `demo` runs on the in-memory mock (simulated results); anything else talks to the bridge API. */
export const DEMO = import.meta.env.VITE_BRIDGE_MODE === 'demo'
/** Public https:// origin of the bridge API, reached only through Nym. Unset = live requests fail (no fallback). */
export const API_BASE = (import.meta.env.VITE_API_BASE ?? '').replace(/\/$/, '')
export const POLL_MS = DEMO ? 1000 : 5000
/** Sepolia; only used until /v1/info reports the active chain. */
export const DEFAULT_CHAIN_ID = '11155111'
export const explorerTx = (hash: string) => `https://sepolia.etherscan.io/tx/${hash}`
export const explorerAddress = (addr: string) => `https://sepolia.etherscan.io/address/${addr}`
