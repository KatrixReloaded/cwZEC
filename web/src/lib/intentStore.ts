// Minimal reload recovery for a redemption: identity and public chain references only.
// Never the destination, amount or calldata.
export interface Scope {
  user: string
  chainId: string
  contract: string
}
export interface SavedIntent {
  opId: string
  hash?: string
  redeemId?: string
}

const key = (s: Scope) => `maze.redeem:${s.chainId}:${s.contract.toLowerCase()}:${s.user.toLowerCase()}`
const str = (v: unknown) => (typeof v === 'string' && v.length <= 128 ? v : undefined)

export const intentStore = {
  load(s: Scope): SavedIntent | null {
    try {
      const v = JSON.parse(localStorage.getItem(key(s)) ?? 'null')
      return str(v?.opId) ? { opId: v.opId, hash: str(v.hash), redeemId: str(v.redeemId) } : null
    } catch {
      return null
    }
  },
  save(s: Scope, v: SavedIntent) {
    try {
      localStorage.setItem(key(s), JSON.stringify({ opId: v.opId, hash: v.hash, redeemId: v.redeemId }))
    } catch {}
  },
  clear(s: Scope) {
    try {
      localStorage.removeItem(key(s))
    } catch {}
  },
}
export type IntentStore = typeof intentStore
