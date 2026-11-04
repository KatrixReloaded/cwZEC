/** What we send through the tunnel; `signal`, `credentials` and `redirect` are not supported by mix-fetch. */
export interface MixInit {
  method?: string
  headers: Record<string, string>
  body?: string
}
type MixFetch = (url: string, init: MixInit) => Promise<Response>
export interface NymState {
  phase: 'idle' | 'loading' | 'connecting' | 'ready' | 'failed'
  since: number
  pending: number
  waitingSince: number
  requestFailed: boolean
}

/** Tunnel setup failed. It is one-shot per page, so only a reload can retry setup. */
export class TunnelError extends Error {}
/** Too many requests are still in flight (including abandoned ones that haven't settled). */
export class TunnelBusy extends Error {}

/** Abandoned requests keep their slot until mix-fetch settles them; excess requests are rejected, not queued. */
export const MAX_OUTSTANDING = 6

const load = async (connecting: () => void): Promise<MixFetch> => {
  const m = await import('@nymproject/mix-fetch')
  connecting()
  // Default mixing and cover traffic. No redirects: a 3xx with Location rejects, like `redirect: 'error'`.
  await m.setupMixTunnel({ maxRedirects: 0 })
  return m.mixFetch
}

export function createTransport(setup: (connecting: () => void) => Promise<MixFetch> = load) {
  let tunnel: Promise<MixFetch> | undefined
  let outstanding = 0
  let failure: TunnelError | undefined
  let state: NymState = { phase: 'idle', since: 0, pending: 0, waitingSince: 0, requestFailed: false }
  const listeners = new Set<() => void>()
  const update = (patch: Partial<NymState>) => {
    state = { ...state, ...patch }
    for (const listener of listeners) listener()
  }

  return {
    /** Starts the shared tunnel once per page; later calls reuse it. */
    connect: () => {
      if (failure) return Promise.reject(failure)
      if (!tunnel) {
        update({ phase: 'loading', since: Date.now() })
        tunnel = setup(() => update({ phase: 'connecting', since: Date.now() })).then(f => {
          update({ phase: 'ready', waitingSince: Date.now() })
          return f
        }).catch(cause => {
          failure ??= new TunnelError('Nym tunnel failed to start. Reload to reconnect.', { cause })
          update({ phase: 'failed' })
          throw failure
        })
      }
      return tunnel
    },

    getSnapshot: () => state,
    subscribe: (listener: () => void) => { listeners.add(listener); return () => { listeners.delete(listener) } },

    outstanding: () => outstanding,

    /**
     * mix-fetch can't cancel: aborting rejects for the caller and drops the late result,
     * but the request may still reach the API and keeps its slot until it settles.
     */
    fetch(url: string, init: MixInit, signal?: AbortSignal): Promise<Response> {
      if (signal?.aborted) return Promise.reject(signal.reason)
      if (failure) return Promise.reject(failure)
      if (outstanding >= MAX_OUTSTANDING) return Promise.reject(new TunnelBusy('Too many requests in flight'))
      outstanding++
      update({ pending: outstanding, ...(outstanding === 1 ? { waitingSince: Date.now() } : {}) })
      const req = this.connect().then((f) => {
        // Startup can take seconds; don't dispatch abandoned requests afterward.
        if (signal?.aborted) throw signal.reason
        if (failure) throw failure
        return f(url, init)
      })
      req.then(
        () => update({ pending: --outstanding, requestFailed: false }),
        () => update({ pending: --outstanding, ...(!signal?.aborted ? { requestFailed: true } : {}) }),
      )
      if (!signal) return req
      return new Promise((resolve, reject) => {
        // A caller's deadline says nothing about the shared tunnel's health.
        const abort = () => {
          if (signal.reason?.name === 'TimeoutError') update({ requestFailed: true })
          reject(signal.reason)
        }
        signal.addEventListener('abort', abort, { once: true })
        req.then(resolve, reject).finally(() => signal.removeEventListener('abort', abort))
      })
    },
  }
}

export const nym = createTransport()
