import type { BridgeApi, PrepareResponse } from '../api/types'
import type { IntentStore, SavedIntent, Scope } from './intentStore'
import { prepareBody, readRedeemReceipt, verifyPrepared, type Receipt, type WalletTx } from './redeemTx'
import { WalletMismatchError, walletNotSent } from './walletErrors'

export type Phase =
  | 'idle'
  | 'preparing' | 'prepareFailed' | 'invalid'
  | 'wallet' | 'cancelled' | 'mismatch' | 'unknown'
  | 'confirming' | 'reverted' | 'noEvent'
  | 'tracking' | 'unfinished'

export interface Terms {
  /** integer zatoshis */
  amountZat: string
  zaddr: string
}
/** Restored intents have no `terms`: amount and destination are never stored. */
export interface Intent extends SavedIntent, Scope {
  terms?: Terms
}
export interface FlowState {
  phase: Phase
  intent: Intent | null
}
export interface FlowDeps {
  api: Pick<BridgeApi, 'prepareRedeem'>
  chain: { sendTransaction(tx: WalletTx): Promise<string> }
  store: IntentStore
  newId?: () => string
  prepareTimeoutMs?: number
}

/** Phases where nothing was sent, so the same intent may be prepared and offered again. */
const RETRYABLE = new Set<Phase>(['prepareFailed', 'cancelled', 'mismatch'])
/** Phases where editing the form may drop the intent: it never reached the network. */
const UNSENT = new Set<Phase>(['prepareFailed', 'invalid', 'cancelled', 'mismatch'])

const scopeId = (s: Scope) => `${s.chainId}:${s.contract}:${s.user}`.toLowerCase()
const sameTerms = (a?: Terms, b?: Terms) => !!a && !!b && a.amountZat === b.amountZat && a.zaddr === b.zaddr
const timeout = <T>(p: Promise<T>, ms: number) =>
  new Promise<T>((resolve, reject) => {
    const t = setTimeout(() => reject(new Error('timeout')), ms)
    p.then(resolve, reject).finally(() => clearTimeout(t))
  })

/**
 * One redemption at a time per account/chain/contract: prepare (idempotent per operation ID),
 * verify, send once through the wallet, then wait for the receipt's RedeemRequested event.
 * Once a hash is known it is kept and never sent again.
 */
export function createRedeemFlow(deps: FlowDeps, emit: (s: FlowState) => void) {
  let state: FlowState = { phase: 'idle', intent: null }
  let scope: Scope | null = null
  let busy = false
  // The transport can't cancel a POST, so a prepare that timed out stays pending; a retry of the same
  // operation waits on it instead of sending an overlapping request.
  let pending: { opId: string; req: Promise<PrepareResponse> } | null = null
  const set = (p: Partial<FlowState>) => emit((state = { ...state, ...p }))
  const save = (it: Intent) => deps.store.save(it, it)
  const newId = deps.newId ?? (() => `web-${crypto.randomUUID()}`)

  async function run(it: Intent) {
    busy = true
    // Late results for another account/network are persisted but not shown.
    const live = () => !!scope && scopeId(scope) === scopeId(it)
    save(it)
    set({ intent: it, phase: 'preparing' })
    try {
      const terms = { opId: it.opId, user: it.user, contract: it.contract, chainId: it.chainId, ...it.terms! }
      let res
      try {
        if (pending?.opId !== it.opId) {
          const p = (pending = { opId: it.opId, req: deps.api.prepareRedeem(prepareBody(terms)) })
          p.req.catch(() => {}).finally(() => pending === p && (pending = null))
        }
        res = await timeout(pending.req, deps.prepareTimeoutMs ?? 60_000)
      } catch {
        // A timeout doesn't mean the backend discarded it; a retry reuses the same ID and terms.
        if (live()) set({ phase: 'prepareFailed' })
        return
      }
      let v: ReturnType<typeof verifyPrepared>
      try {
        v = verifyPrepared(res, terms)
      } catch {
        if (live()) set({ phase: 'invalid' })
        return
      }
      let hash: string | undefined = v.kind === 'hash' ? v.hash : undefined
      if (!hash && v.kind === 'send') {
        if (!live()) return // never open a wallet prompt for an account that is no longer selected
        set({ phase: 'wallet' })
        try {
          hash = await deps.chain.sendTransaction(v.tx)
        } catch (e) {
          // Only a definite non-send may be retried; anything else is never re-sent automatically.
          if (live()) set({ phase: e instanceof WalletMismatchError ? 'mismatch' : walletNotSent(e) ? 'cancelled' : 'unknown' })
          return
        }
      }
      const done = { ...it, hash }
      save(done)
      if (live()) set({ intent: done, phase: 'confirming' })
    } finally {
      busy = false
    }
  }

  return {
    get state() {
      return state
    },
    get busy() {
      return busy
    },
    /** Switches to an account/chain/contract and restores its saved intent, if any. */
    setScope(next: Scope | null) {
      if (next && scope && scopeId(next) === scopeId(scope)) return
      scope = next
      const saved = next ? deps.store.load(next) : null
      const intent = saved && next ? { ...saved, ...next } : null
      set({ intent, phase: !intent ? 'idle' : intent.redeemId ? 'tracking' : intent.hash ? 'confirming' : 'unfinished' })
    },
    /** Explicit user action. Reuses the current intent after a definite non-send with the same terms. */
    start(terms: Terms) {
      if (busy || !scope || !(state.phase === 'idle' || RETRYABLE.has(state.phase))) return
      const it = state.intent && RETRYABLE.has(state.phase) && sameTerms(state.intent.terms, terms) ? state.intent : { ...scope, opId: newId(), terms }
      return run(it)
    },
    retry() {
      if (!busy && state.intent?.terms && RETRYABLE.has(state.phase)) return run(state.intent)
    },
    /** The form changed after a definite non-send: forget that intent. */
    discardUnsent() {
      if (!busy && state.intent && !state.intent.hash && UNSENT.has(state.phase)) {
        deps.store.clear(state.intent)
        set({ intent: null, phase: 'idle' })
      }
    },
    /** "New redeem": forget the current intent. Refused while a wallet request is open. */
    reset() {
      if (busy) return
      if (state.intent) deps.store.clear(state.intent)
      set({ intent: null, phase: 'idle' })
    },
    receipt(r: Receipt) {
      const it = state.intent
      if (state.phase !== 'confirming' || !it?.hash) return
      try {
        const out = readRedeemReceipt(r, it.hash, it)
        if (out.status === 'reverted') return set({ phase: 'reverted' })
        const done = { ...it, redeemId: out.redeemId }
        save(done)
        set({ intent: done, phase: 'tracking' })
      } catch {
        set({ phase: 'noEvent' })
      }
    },
  }
}

export type RedeemFlow = ReturnType<typeof createRedeemFlow>
