import { describe, expect, it, vi } from 'vitest'
import { createLatest } from './latest'

describe('createLatest (memo per recipient)', () => {
  it('drops a slow answer for a previous recipient and aborts its request', async () => {
    const resolvers = new Map<string, (v: string) => void>()
    const signals = new Map<string, AbortSignal>()
    const emit = vi.fn()
    const l = createLatest<string>((key, signal) => (signals.set(key, signal), new Promise((r) => resolvers.set(key, r))), emit)
    l.request('0xaaa')
    l.request('0xbbb')
    expect(signals.get('0xaaa')!.aborted).toBe(true)
    resolvers.get('0xbbb')!('memo-b')
    resolvers.get('0xaaa')!('memo-a')
    await Promise.resolve()
    await Promise.resolve()
    expect(emit.mock.calls).toEqual([['0xbbb', { ok: true, value: 'memo-b' }]])
  })

  it('reports failure for the current key and nothing after cancel', async () => {
    const emit = vi.fn()
    const l = createLatest<string>(() => Promise.reject(new Error('down')), emit)
    l.request('0xaaa')
    await new Promise((r) => setTimeout(r))
    expect(emit).toHaveBeenCalledWith('0xaaa', { ok: false })
    emit.mockClear()
    l.request('0xbbb')
    l.cancel()
    l.request(null)
    await new Promise((r) => setTimeout(r))
    expect(emit).not.toHaveBeenCalled()
  })
})
