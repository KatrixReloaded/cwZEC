import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { getJson } from './http'
import { MAX_OUTSTANDING, TunnelBusy, TunnelError, createTransport, nym } from './nym'

vi.mock('../config', async (orig) => ({ ...(await orig<object>()), API_BASE: '' }))

const ok = () => new Response('{}', { status: 200 })
/** A mix-fetch stand-in whose requests settle only when the test says so. */
function deferredFetch() {
  const calls: { url: string; resolve: (r: Response) => void; reject: (e: Error) => void }[] = []
  const fn = vi.fn((url: string) => new Promise<Response>((resolve, reject) => calls.push({ url, resolve, reject })))
  return { fn, calls }
}
const tick = () => new Promise((r) => setTimeout(r))

beforeEach(() => vi.stubGlobal('fetch', vi.fn(() => Promise.reject(new Error('native fetch used')))))
afterEach(() => {
  expect(fetch).not.toHaveBeenCalled()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('Nym transport', () => {
  it('sets the tunnel up once for concurrent and later requests', async () => {
    const mix = vi.fn(async (_url: string, _init?: unknown) => ok())
    const setup = vi.fn(async () => mix)
    const t = createTransport(setup)
    await Promise.all([t.fetch('https://a/1', { headers: {} }), t.fetch('https://a/2', { headers: {} })])
    await t.fetch('https://a/3', { headers: {} })
    expect(setup).toHaveBeenCalledTimes(1)
    expect(mix.mock.calls.map((c) => c[0])).toEqual(['https://a/1', 'https://a/2', 'https://a/3'])
  })

  it('fails every request after a setup failure without retrying setup or using native fetch', async () => {
    const setup = vi.fn(() => Promise.reject(new Error('no gateway')))
    const t = createTransport(setup)
    await expect(t.fetch('https://a/1', { headers: {} })).rejects.toBeInstanceOf(TunnelError)
    await expect(t.fetch('https://a/2', { headers: {} })).rejects.toThrow('Reload to reconnect')
    expect(setup).toHaveBeenCalledTimes(1)
    expect(t.outstanding()).toBe(0)
  })

  it('passes request failures through without falling back', async () => {
    const t = createTransport(async () => () => Promise.reject(new Error('IPR refused destination')))
    await expect(t.fetch('https://a/1', { headers: {} })).rejects.toThrow('IPR refused destination')
  })

  it('never hands the AbortSignal to mix-fetch and rejects a request aborted before dispatch', async () => {
    const mix = vi.fn(async (_url: string, _init?: unknown) => ok())
    const t = createTransport(async () => mix)
    await t.fetch('https://a/1', { method: 'POST', headers: { a: 'b' }, body: '{}' }, new AbortController().signal)
    expect(mix.mock.calls[0]).toEqual(['https://a/1', { method: 'POST', headers: { a: 'b' }, body: '{}' }])
    const c = new AbortController()
    c.abort()
    await expect(t.fetch('https://a/2', { headers: {} }, c.signal)).rejects.toBeDefined()
    expect(mix).toHaveBeenCalledTimes(1)
  })

  it('drops the result of an aborted request but counts it until it settles', async () => {
    const { fn, calls } = deferredFetch()
    const t = createTransport(async () => fn)
    const c = new AbortController()
    const res = vi.fn()
    const p = t.fetch('https://a/1', { headers: {} }, c.signal).then(res, (e) => e)
    await tick()
    c.abort()
    expect(await p).toBe(c.signal.reason)
    expect(t.outstanding()).toBe(1)
    calls[0].resolve(ok())
    await tick()
    expect(res).not.toHaveBeenCalled()
    expect(t.outstanding()).toBe(0)
  })

  it('rejects requests beyond the bound without queueing, including abandoned ones', async () => {
    const { fn, calls } = deferredFetch()
    const t = createTransport(async () => fn)
    const c = new AbortController()
    const held = Array.from({ length: MAX_OUTSTANDING }, () => t.fetch('https://a/x', { headers: {} }, c.signal).catch(() => {}))
    await tick() // All calls have reached the SDK, so abort cannot cancel them.
    c.abort()
    await Promise.all(held)
    await expect(t.fetch('https://a/over', { headers: {} })).rejects.toBeInstanceOf(TunnelBusy)
    expect(fn).toHaveBeenCalledTimes(MAX_OUTSTANDING)
    calls[0].reject(new Error('late failure'))
    await tick()
    void t.fetch('https://a/next', { headers: {} })
    await tick()
    expect(fn).toHaveBeenCalledTimes(MAX_OUTSTANDING + 1)
  })

  it('does not dispatch a request cancelled while the shared tunnel is starting', async () => {
    let ready!: (fetch: typeof mix) => void
    const mix = vi.fn(async () => ok())
    const t = createTransport(() => new Promise((resolve) => { ready = resolve }))
    const c = new AbortController()
    const abandoned = t.fetch('https://a/old', { headers: {} }, c.signal).catch((e) => e)
    c.abort()
    expect(await abandoned).toBe(c.signal.reason)
    const current = t.fetch('https://a/current', { headers: {} })
    ready(mix)
    await current
    expect(mix).toHaveBeenCalledTimes(1)
    expect(mix).toHaveBeenCalledWith('https://a/current', { headers: {} })
    expect(t.outstanding()).toBe(0)
  })

  it('marks a timed-out tunnel unavailable and stops dispatching retries even after a late result', async () => {
    const { fn, calls } = deferredFetch()
    const t = createTransport(async () => fn)
    const c = new AbortController()
    const stalled = t.fetch('https://a/status', { headers: {} }, c.signal).catch((e) => e)
    await tick()
    c.abort(new DOMException('deadline', 'TimeoutError'))
    expect(await stalled).toBeInstanceOf(TunnelError)
    expect(t.outstanding()).toBe(1)
    await expect(t.fetch('https://a/retry', { headers: {} })).rejects.toBeInstanceOf(TunnelError)
    calls[0].resolve(ok())
    await tick()
    expect(t.outstanding()).toBe(0)
    await expect(t.fetch('https://a/retry', { headers: {} })).rejects.toBeInstanceOf(TunnelError)
    expect(fn).toHaveBeenCalledTimes(1)
  })

  it('a setup timeout prevents queued requests from being dispatched when setup eventually finishes', async () => {
    const mix = vi.fn(async () => ok())
    let ready!: (fetch: typeof mix) => void
    const t = createTransport(() => new Promise((resolve) => { ready = resolve }))
    const c = new AbortController()
    const first = t.fetch('https://a/info', { headers: {} }, c.signal).catch((e) => e)
    const queued = t.fetch('https://a/memo', { headers: {} }).catch((e) => e)
    c.abort(new DOMException('deadline', 'TimeoutError'))
    expect(await first).toBeInstanceOf(TunnelError)
    ready(mix)
    expect(await queued).toBeInstanceOf(TunnelError)
    expect(mix).not.toHaveBeenCalled()
    expect(t.outstanding()).toBe(0)
  })
})

describe('http without an API origin', () => {
  it('refuses live requests instead of fetching anywhere', async () => {
    const spy = vi.spyOn(nym, 'fetch')
    await expect(getJson('/v1/info')).rejects.toMatchObject({ status: 0, code: 'api_not_configured' })
    expect(spy).not.toHaveBeenCalled()
  })
})
