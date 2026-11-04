import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { startPolling } from './usePoll'

beforeEach(() => vi.useFakeTimers())
afterEach(() => vi.useRealTimers())

const deferred = <T,>() => {
  let resolve!: (v: T) => void, reject!: (e: unknown) => void
  const promise = new Promise<T>((a, b) => ((resolve = a), (reject = b)))
  return { promise, resolve, reject }
}

describe('startPolling', () => {
  it('fetches immediately, then waits for each request to settle before scheduling the next', async () => {
    const pending = [deferred<number>(), deferred<number>()]
    const fetch = vi.fn((_: AbortSignal) => pending[fetch.mock.calls.length - 1].promise)
    const results: unknown[] = []
    const stop = startPolling(fetch, (r) => results.push(r), 5000)

    expect(fetch).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(20_000) // slow request: no overlapping calls
    expect(fetch).toHaveBeenCalledTimes(1)

    pending[0].resolve(1)
    await vi.advanceTimersByTimeAsync(4999)
    expect(fetch).toHaveBeenCalledTimes(1)
    await vi.advanceTimersByTimeAsync(1)
    expect(fetch).toHaveBeenCalledTimes(2)
    expect(results).toEqual([{ ok: true, data: 1 }])
    stop()
  })

  it('keeps polling after errors', async () => {
    const fetch = vi.fn().mockRejectedValueOnce(new Error('down')).mockResolvedValue('up')
    const results: unknown[] = []
    const stop = startPolling(fetch, (r) => results.push(r), 1000)
    await vi.advanceTimersByTimeAsync(1000)
    expect(results).toEqual([{ ok: false, error: new Error('down') }, { ok: true, data: 'up' }])
    stop()
  })

  it('aborts the in-flight request on stop and drops its late result', async () => {
    const d = deferred<string>()
    let signal!: AbortSignal
    const fetch = vi.fn((s: AbortSignal) => ((signal = s), d.promise))
    const onResult = vi.fn()
    const stop = startPolling(fetch, onResult, 1000)
    stop()
    expect(signal.aborted).toBe(true)
    d.resolve('stale')
    await vi.advanceTimersByTimeAsync(5000)
    expect(onResult).not.toHaveBeenCalled()
    expect(fetch).toHaveBeenCalledTimes(1)
  })

  it('stops scheduling once stopped between polls', async () => {
    const fetch = vi.fn().mockResolvedValue(1)
    const stop = startPolling(fetch, () => {}, 1000)
    await vi.advanceTimersByTimeAsync(0)
    stop()
    await vi.advanceTimersByTimeAsync(10_000)
    expect(fetch).toHaveBeenCalledTimes(1)
  })
})
