import { useEffect, useState, useSyncExternalStore } from 'react'
import { nym, type NymState } from '../api/nym'

export function nymStatusLabel(s: NymState, elapsed: number) {
  if (s.phase === 'idle') return 'Nym · Not started'
  if (s.phase === 'loading') return 'Nym · Loading privacy connection…'
  if (s.phase === 'connecting') return elapsed >= 30 ? 'Nym · Still connecting…' : 'Nym · Connecting…'
  if (s.phase === 'failed') return 'Nym · Connection failed'
  if (s.pending) return elapsed >= 15 ? 'Nym · Response taking longer…' : 'Nym · Waiting for response…'
  if (s.requestFailed) return 'Nym · Request failed; connection may still be available'
  return 'Nym · Ready'
}

export function NymStatus() {
  const state = useSyncExternalStore(nym.subscribe, nym.getSnapshot, nym.getSnapshot)
  const [clock, setClock] = useState(Date.now)
  const starting = state.phase === 'loading' || state.phase === 'connecting'
  const waiting = starting || (state.phase === 'ready' && state.pending > 0)
  useEffect(() => {
    if (!waiting) return
    const timer = setInterval(() => setClock(Date.now()), 1000)
    return () => clearInterval(timer)
  }, [waiting])
  const elapsed = Math.max(0, Math.floor((clock - (starting ? state.since : state.waitingSince)) / 1000))
  return <div className="nym-status">
    <span role="status" aria-live="polite">{nymStatusLabel(state, elapsed)}</span>
    {waiting && <span aria-hidden="true">{elapsed}s</span>}
    {state.phase === 'failed' && <button className="btn btn-t" onClick={() => location.reload()}>Reload</button>}
  </div>
}
