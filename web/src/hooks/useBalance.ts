import { useEffect, useRef, useState } from 'react'
import { createRevealer, type BalanceView } from '../lib/balanceReveal'
import { chain, type RevealScope } from '../wallet/chain'

export type { BalanceState } from '../lib/balanceReveal'

/** `scope` is null unless the wallet is connected on the bridge's chain and the contract is known. */
export function useBalance(scope: RevealScope | null) {
  const [view, setView] = useState<BalanceView>({ state: 'encrypted', amount: null })
  const r = useRef<ReturnType<typeof createRevealer<RevealScope>> | null>(null)
  r.current ??= createRevealer((s, onPhase) => chain.revealBalance(s, onPhase), setView)
  const key = scope ? `${scope.account}:${scope.chainId}:${scope.contract}`.toLowerCase() : null

  useEffect(() => r.current!.setScope(scope), [key]) // eslint-disable-line react-hooks/exhaustive-deps

  return {
    ...view,
    /** Zatoshis, only while revealed. */
    known: view.state === 'revealed' ? view.amount : null,
    canReveal: !!scope,
    reveal: () => r.current!.reveal(),
    hide: () => r.current!.hide(),
  }
}

export type Balance = ReturnType<typeof useBalance>
