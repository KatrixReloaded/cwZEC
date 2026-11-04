import { afterEach, describe, expect, it, vi } from 'vitest'
import { intentStore } from './intentStore'

const scope = { user: '0xAB', chainId: '11155111', contract: '0xCD' }

function stubStorage() {
  const m = new Map<string, string>()
  vi.stubGlobal('localStorage', { getItem: (k: string) => m.get(k) ?? null, setItem: (k: string, v: string) => void m.set(k, v), removeItem: (k: string) => void m.delete(k) })
  return m
}
afterEach(() => vi.unstubAllGlobals())

describe('intentStore', () => {
  it('stores only identity and public references, scoped to account/chain/contract', () => {
    const m = stubStorage()
    intentStore.save(scope, { opId: 'web-1', hash: '0x1', terms: { amountZat: '5', zaddr: 'utest1secret' } } as never)
    expect([...m.entries()]).toEqual([['maze.redeem:11155111:0xcd:0xab', '{"opId":"web-1","hash":"0x1"}']])
    expect(intentStore.load({ ...scope, user: '0xab' })).toEqual({ opId: 'web-1', hash: '0x1', redeemId: undefined })
    expect(intentStore.load({ ...scope, user: '0xEF' })).toBeNull()
    intentStore.clear(scope)
    expect(m.size).toBe(0)
  })

  it('tolerates missing, corrupt or throwing storage', () => {
    const m = stubStorage()
    m.set('maze.redeem:11155111:0xcd:0xab', '{nope')
    expect(intentStore.load(scope)).toBeNull()
    vi.stubGlobal('localStorage', { getItem: () => { throw new Error('denied') }, setItem: () => { throw new Error('denied') }, removeItem: () => { throw new Error('denied') } })
    expect(intentStore.load(scope)).toBeNull()
    expect(() => intentStore.save(scope, { opId: 'x' })).not.toThrow()
  })
})
