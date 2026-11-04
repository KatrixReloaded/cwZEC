import { useEffect, useState } from 'react'
import { POLL_MS } from '../config'

type Result<T> = { ok: true; data: T } | { ok: false; error: unknown }

/**
 * Calls `fetch` now and again `ms` after each attempt settles (never overlapping) until stopped.
 * Stopping aborts the in-flight request and drops its result.
 */
export function startPolling<T>(fetch: (signal: AbortSignal) => Promise<T>, onResult: (r: Result<T>) => void, ms = POLL_MS) {
  let stopped = false
  let timer: ReturnType<typeof setTimeout> | undefined
  let ctrl: AbortController | undefined
  const tick = async () => {
    ctrl = new AbortController()
    let r: Result<T>
    try {
      r = { ok: true, data: await fetch(ctrl.signal) }
    } catch (error) {
      r = { ok: false, error }
    }
    if (stopped) return
    onResult(r)
    timer = setTimeout(tick, ms)
  }
  tick()
  return () => {
    stopped = true
    clearTimeout(timer)
    ctrl?.abort()
  }
}

export interface Polled<T> {
  data?: T
  /** last request failed; `data`, if any, is from an earlier success */
  error?: unknown
  loading: boolean
  stale: boolean
}

/** Polls `fetch(key)` while `key` is non-null; a new key discards everything from the old one. */
export function usePoll<T>(key: string | null, fetch: (key: string, signal: AbortSignal) => Promise<T>): Polled<T> {
  const [s, setS] = useState<{ key: string | null; data?: T; error?: unknown }>({ key })

  useEffect(() => {
    setS({ key })
    if (key == null) return
    return startPolling(
      (signal) => fetch(key, signal),
      (r) => setS((p) => (r.ok ? { key, data: r.data } : { key, data: p.data, error: r.error })),
    )
  }, [key]) // eslint-disable-line react-hooks/exhaustive-deps

  // Until the effect resets state, ignore anything belonging to a previous key.
  const cur = s.key === key ? s : { key }
  return {
    data: cur.data,
    error: cur.error,
    loading: key != null && cur.data === undefined && cur.error === undefined,
    stale: cur.error !== undefined && cur.data !== undefined,
  }
}
