/** What we send through the tunnel; `signal`, `credentials` and `redirect` are not supported by mix-fetch. */
export interface MixInit {
  method?: string
  headers: Record<string, string>
  body?: string
}
type MixFetch = (url: string, init: MixInit) => Promise<Response>

/** The tunnel failed or stalled. It is one-shot per page, so only a reload can retry. */
export class TunnelError extends Error {}
/** Too many requests are still in flight (including abandoned ones that haven't settled). */
export class TunnelBusy extends Error {}

/** Abandoned requests keep their slot until mix-fetch settles them; excess requests are rejected, not queued. */
export const MAX_OUTSTANDING = 6

const load = async (): Promise<MixFetch> => {
  const m = await import('@nymproject/mix-fetch')
  // Default mixing and cover traffic. No redirects: a 3xx with Location rejects, like `redirect: 'error'`.
  await m.setupMixTunnel({ maxRedirects: 0 })
  return m.mixFetch
}

export function createTransport(setup: () => Promise<MixFetch> = load) {
  let tunnel: Promise<MixFetch> | undefined
  let outstanding = 0
  let failure: TunnelError | undefined

  return {
    /** Starts the shared tunnel once per page; later calls reuse it. */
    connect: () => failure ? Promise.reject(failure) :
      (tunnel ??= setup().catch(() => {
        failure ??= new TunnelError('Nym tunnel failed to start. Reload to reconnect.')
        throw failure
      })),

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
      const req = this.connect().then((f) => {
        // Startup can take seconds; don't dispatch abandoned requests afterward.
        if (signal?.aborted) throw signal.reason
        if (failure) throw failure
        return f(url, init)
      })
      req.then(
        () => outstanding--,
        () => outstanding--,
      )
      if (!signal) return req
      return new Promise((resolve, reject) => {
        const abort = () => {
          if (signal.reason?.name === 'TimeoutError') {
            failure ??= new TunnelError('Nym connection stalled. Reload to reconnect.')
          }
          reject(failure ?? signal.reason)
        }
        signal.addEventListener('abort', abort, { once: true })
        req.then(resolve, reject).finally(() => signal.removeEventListener('abort', abort))
      })
    },
  }
}

export const nym = createTransport()
