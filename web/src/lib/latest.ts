type Outcome<T> = { ok: true; value: T } | { ok: false }

/** Runs one keyed request at a time: a new key (or cancel) aborts the previous one and drops its result. */
export function createLatest<T>(load: (key: string, signal: AbortSignal) => Promise<T>, emit: (key: string, r: Outcome<T>) => void) {
  let ctrl: AbortController | null = null
  const cancel = () => ctrl?.abort()
  return {
    request(key: string | null) {
      cancel()
      if (key == null) return
      const c = (ctrl = new AbortController())
      load(key, c.signal).then(
        (value) => !c.signal.aborted && emit(key, { ok: true, value }),
        () => !c.signal.aborted && emit(key, { ok: false }),
      )
    },
    cancel,
  }
}
