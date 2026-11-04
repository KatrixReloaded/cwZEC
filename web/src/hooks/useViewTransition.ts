import { useEffect, useRef, useState } from 'react'
import type { Route } from './useHashRoute'

export type View = 'deposit' | 'redeem' | 'swap'
export type ViewPhase = 'in' | 'pre' | 'out' | 'gone'
type Phases = Record<View, ViewPhase>

const only = (v: Route): Phases => ({ swap: v === 'swap' ? 'in' : 'gone', deposit: v === 'deposit' ? 'in' : 'gone', redeem: v === 'redeem' ? 'in' : 'gone' })
const reducedMotion = () => window.matchMedia?.('(prefers-reduced-motion: reduce)').matches

/**
 * Deposit <-> Redeem: the old view's sections fade out while the new one's stagger in, under a
 * grid-sweep veil. Latest choice wins and reverses mid-flight.
 */
export function useViewTransition(route: Route) {
  const [phases, setPhasesState] = useState(() => only(route))
  const [veil, setVeil] = useState<{ key: number; dir: 'fwd' | 'rev' } | null>(null)
  const ref = useRef({ phases, prev: route, timers: [] as number[], key: 0 })

  useEffect(() => {
    const s = ref.current
    const from = s.prev
    s.prev = route
    if (from === route || route === 'notfound') return
    s.timers.forEach(clearTimeout)
    s.timers = []
    const set = (p: Phases) => setPhasesState((s.phases = p))
    const later = (ms: number, fn: () => void) => s.timers.push(window.setTimeout(fn, ms))

    if (from === 'notfound' || reducedMotion()) return set(only(route))

    const to = route as View, old = from as View
    const next = { ...s.phases }
    const pre = next[to] === 'gone'
    next[to] = pre ? 'pre' : 'in'
    next[old] = next[old] === 'pre' || next[old] === 'gone' ? 'gone' : 'out'
    set(next)
    setVeil({ key: ++s.key, dir: to === 'redeem' || (to === 'swap' && from === 'deposit') ? 'fwd' : 'rev' })

    if (pre) later(300, () => s.phases[to] === 'pre' && set({ ...s.phases, [to]: 'in' }))
    later(720, () => s.phases[old] === 'out' && set({ ...s.phases, [old]: 'gone' }))
    // A rapid third navigation cancels prior timers; retire any earlier outgoing view too.
    for (const v of ['deposit', 'redeem', 'swap'] as const) {
      if (v !== old && v !== to) later(720, () => s.phases[v] !== 'gone' && set({ ...s.phases, [v]: 'gone' }))
    }
    later(900, () => setVeil(null))
  }, [route])

  useEffect(() => () => ref.current.timers.forEach(clearTimeout), [])

  return { phases, veil }
}
