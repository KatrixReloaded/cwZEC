import { getAddress } from 'viem'
import { useEffect, useRef, useState } from 'react'
import { createRevealer, type BalanceView } from '../lib/balanceReveal'
import { metamask } from '../wallet/metamask'
import type { RevealScope } from '../wallet/chain'
import { session } from './chain'
/** Swap-only reveal lifecycle also clears immediately on raw wallet events. */
export function usePrivateBalance(scope: RevealScope | null) {
  const [view, setView] = useState<BalanceView>({ state: 'encrypted', amount: null })
  const abort = useRef<AbortController | null>(null)
  const revealer = useRef<ReturnType<typeof createRevealer<RevealScope>> | null>(null)
  revealer.current ??= createRevealer(async (s, phase) => {
    const signal = abort.current?.signal
    await session(s.account)
    const { revealBalance } = await import('./fhe')
    const value = await revealBalance(s.account, getAddress(s.contract), phase, signal)
    await session(s.account)
    return value
  }, setView)
  const key = scope ? `${scope.account}:${scope.chainId}:${scope.contract}`.toLowerCase() : null
  useEffect(() => {
    const r = revealer.current!
    r.setScope(scope)
    const off = metamask.subscribe(() => { abort.current?.abort(); r.hide() })
    return () => { abort.current?.abort(); r.setScope(null); off() }
  }, [key]) // The key includes all scope fields.
  return { ...view, known: view.state === 'revealed' ? view.amount : null, canReveal: !!scope,
    reveal: () => { abort.current?.abort(); abort.current = new AbortController(); return revealer.current!.reveal() },
    hide: () => { abort.current?.abort(); revealer.current!.hide() } }
}
