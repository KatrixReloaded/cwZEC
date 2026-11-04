import { describe, expect, it } from 'vitest'
import { createRevealer, type BalanceView } from './balanceReveal'

const deferred = <T,>() => {
  let resolve!: (v: T) => void, reject!: (e: unknown) => void
  const promise = new Promise<T>((a, b) => ((resolve = a), (reject = b)))
  return { promise, resolve, reject }
}

function setup() {
  const views: BalanceView[] = []
  let pending = deferred<bigint>()
  const r = createRevealer<string>((_s, onPhase) => {
    pending = deferred<bigint>()
    onPhase('decrypting')
    return pending.promise
  }, (v) => views.push(v))
  return { r, views, last: () => views[views.length - 1], settle: () => pending }
}

describe('balance revealer', () => {
  it('reveals exact zatoshis', async () => {
    const t = setup()
    t.r.setScope('a')
    const p = t.r.reveal()
    t.settle().resolve(1_234_567_891n)
    await p
    expect(t.last()).toEqual({ state: 'revealed', amount: 1_234_567_891n })
  })

  it('clears plaintext on hide and on scope change', async () => {
    const t = setup()
    t.r.setScope('a')
    const p = t.r.reveal()
    t.settle().resolve(5n)
    await p
    t.r.hide()
    expect(t.last()).toEqual({ state: 'encrypted', amount: null })
    const q = t.r.reveal()
    t.settle().resolve(5n)
    await q
    t.r.setScope('b')
    expect(t.last()).toEqual({ state: 'encrypted', amount: null })
  })

  it('drops a reveal that finishes after hide or an account change', async () => {
    const t = setup()
    t.r.setScope('a')
    const p = t.r.reveal()
    const pending = t.settle()
    t.r.setScope('b')
    pending.resolve(99n)
    await p
    expect(t.views.some((v) => v.amount === 99n)).toBe(false)
  })

  it('distinguishes a declined permit from a failure, and does nothing without a scope', async () => {
    const t = setup()
    await t.r.reveal()
    expect(t.views).toEqual([])
    t.r.setScope('a')
    let p = t.r.reveal()
    t.settle().reject({ code: 4001 })
    await p
    expect(t.last().state).toBe('rejected')
    p = t.r.reveal()
    t.settle().reject(new Error('relayer down'))
    await p
    expect(t.last()).toEqual({ state: 'failed', amount: null })
  })
})
