import { walletRejected } from './walletErrors'

export type BalanceState = 'encrypted' | 'signing' | 'decrypting' | 'revealed' | 'failed' | 'rejected'
export interface BalanceView {
  state: BalanceState
  /** zatoshis; only while revealed, kept in memory only */
  amount: bigint | null
}
type Reveal<S> = (scope: S, onPhase: (p: 'signing' | 'decrypting') => void) => Promise<bigint>

/**
 * User-triggered balance reveal bound to one account/chain/contract. Changing scope or hiding
 * drops the plaintext and ignores any reveal still in flight.
 */
export function createRevealer<S>(reveal: Reveal<S>, emit: (v: BalanceView) => void) {
  let run = 0
  let scope: S | null = null
  const set = (state: BalanceState, amount: bigint | null = null) => emit({ state, amount })
  const clear = () => {
    run++
    set('encrypted')
  }
  return {
    setScope(next: S | null) {
      scope = next
      clear()
    },
    hide: clear,
    async reveal() {
      if (!scope) return
      const id = ++run
      set('signing')
      try {
        const n = await reveal(scope, (p) => run === id && set(p))
        if (run === id) set('revealed', n)
      } catch (e) {
        if (run === id) set(walletRejected(e) ? 'rejected' : 'failed')
      }
    },
  }
}
