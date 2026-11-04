import { describe, expect, it, vi } from 'vitest'
import type { PrepareResponse } from '../api/types'
import { CONTRACT, HASH, USER, prepared, receipt } from '../test/redeem'
import type { IntentStore, SavedIntent, Scope } from './intentStore'
import { createRedeemFlow, type FlowState } from './redeemFlow'
import { WalletMismatchError } from './walletErrors'

const scope: Scope = { user: USER, chainId: '11155111', contract: CONTRACT }
const terms = { amountZat: '150000000', zaddr: 'utest1' + 'q'.repeat(60) }

function memStore() {
  const m = new Map<string, SavedIntent>()
  const k = (s: Scope) => `${s.chainId}:${s.contract}:${s.user}`.toLowerCase()
  const store: IntentStore = { load: (s) => m.get(k(s)) ?? null, save: (s, v) => void m.set(k(s), { opId: v.opId, hash: v.hash, redeemId: v.redeemId }), clear: (s) => void m.delete(k(s)) }
  return { store, m }
}

function setup(opts: { prepare?: (n: number) => Promise<PrepareResponse>; send?: () => Promise<string>; prepareTimeoutMs?: number } = {}) {
  let n = 0
  const ids = ['web-1', 'web-2', 'web-3']
  const prepareRedeem = vi.fn((req: { operation_id: string }) => (opts.prepare ? opts.prepare(n++) : Promise.resolve(prepared({ operation_id: req.operation_id }))))
  const sendTransaction = vi.fn(opts.send ?? (() => Promise.resolve(HASH)))
  const { store, m } = memStore()
  const states: FlowState[] = []
  const flow = createRedeemFlow({ api: { prepareRedeem }, chain: { sendTransaction }, store, newId: () => ids.shift()!, prepareTimeoutMs: opts.prepareTimeoutMs }, (s) => states.push(s))
  flow.setScope(scope)
  return { flow, prepareRedeem, sendTransaction, m, states }
}

describe('redeem flow', () => {
  it('prepares, verifies, sends once and keeps the hash', async () => {
    const t = setup()
    await t.flow.start(terms)
    expect(t.prepareRedeem).toHaveBeenCalledWith(
      { operation_id: 'web-1', user: USER, amount: '150000000', zcash_address: terms.zaddr, chain_id: '11155111', contract: CONTRACT },
    )
    expect(t.sendTransaction).toHaveBeenCalledTimes(1)
    expect(t.states.map((s) => s.phase)).toEqual(['idle', 'preparing', 'wallet', 'confirming'])
    expect([...t.m.values()]).toEqual([{ opId: 'web-1', hash: HASH, redeemId: undefined }])
  })

  it('ignores duplicate starts while busy and after a hash is known', async () => {
    const t = setup()
    const first = t.flow.start(terms)
    t.flow.start(terms)
    t.flow.retry()
    await first
    await t.flow.start(terms)
    expect(t.prepareRedeem).toHaveBeenCalledTimes(1)
    expect(t.sendTransaction).toHaveBeenCalledTimes(1)
  })

  it('retries preparation with the same operation ID and terms', async () => {
    const t = setup({ prepare: (n) => (n === 0 ? Promise.reject(new Error('timeout')) : Promise.resolve(prepared())) })
    await t.flow.start(terms)
    expect(t.flow.state.phase).toBe('prepareFailed')
    await t.flow.retry()
    expect(t.prepareRedeem.mock.calls.map((c) => c[0])).toEqual([t.prepareRedeem.mock.calls[0][0], t.prepareRedeem.mock.calls[0][0]])
    expect(t.flow.state.phase).toBe('confirming')
  })

  it('after a timeout, a retry waits on the still-pending prepare instead of sending another', async () => {
    let settle!: (r: PrepareResponse) => void
    const t = setup({ prepare: () => new Promise((r) => (settle = r)), prepareTimeoutMs: 5 })
    await t.flow.start(terms)
    expect(t.flow.state.phase).toBe('prepareFailed')
    const again = t.flow.retry()
    settle(prepared())
    await again
    expect(t.prepareRedeem).toHaveBeenCalledTimes(1)
    expect(t.sendTransaction).toHaveBeenCalledTimes(1)
    expect(t.flow.state).toMatchObject({ phase: 'confirming', intent: { opId: 'web-1', hash: HASH } })
  })

  it('starts a new operation when the terms change after a definite non-send', async () => {
    const t = setup({ send: () => Promise.reject({ code: 4001 }) })
    await t.flow.start(terms)
    expect(t.flow.state.phase).toBe('cancelled')
    await t.flow.start(terms) // same terms: same intent
    t.flow.discardUnsent()
    await t.flow.start({ ...terms, amountZat: '200000000' })
    expect(t.prepareRedeem.mock.calls.map((c) => c[0].operation_id)).toEqual(['web-1', 'web-1', 'web-2'])
  })

  it('maps wallet outcomes: rejection, account change, ambiguous', async () => {
    for (const [err, phase] of [[{ code: 4001 }, 'cancelled'], [new WalletMismatchError(), 'mismatch'], [{ code: -32603 }, 'unknown'], [new Error('disconnected'), 'unknown']] as const) {
      const t = setup({ send: () => Promise.reject(err) })
      await t.flow.start(terms)
      expect(t.flow.state.phase).toBe(phase)
    }
  })

  it('never re-sends after an unknown outcome and keeps the operation for recovery', async () => {
    const t = setup({ send: () => Promise.reject(new Error('timeout')) })
    await t.flow.start(terms)
    t.flow.retry()
    t.flow.discardUnsent()
    await t.flow.start(terms)
    expect(t.sendTransaction).toHaveBeenCalledTimes(1)
    expect(t.flow.state.phase).toBe('unknown')
    expect([...t.m.values()]).toEqual([{ opId: 'web-1', hash: undefined, redeemId: undefined }])
  })

  it('tracks the hash the backend already has instead of sending', async () => {
    const t = setup({ prepare: () => Promise.resolve({ operation_id: 'web-1', already_submitted: true, transaction_hash: HASH }) })
    await t.flow.start(terms)
    expect(t.sendTransaction).not.toHaveBeenCalled()
    expect(t.flow.state).toMatchObject({ phase: 'confirming', intent: { hash: HASH } })
  })

  it('refuses a prepared transaction that does not verify', async () => {
    const t = setup({ prepare: () => Promise.resolve(prepared({}, { value: '0x1' })) })
    await t.flow.start(terms)
    expect(t.flow.state.phase).toBe('invalid')
    expect(t.sendTransaction).not.toHaveBeenCalled()
  })

  it('hands the RedeemRequested ID to tracking, or flags a receipt without it', async () => {
    const t = setup()
    await t.flow.start(terms)
    t.flow.receipt(receipt({}, { id: 9n }))
    expect(t.flow.state).toMatchObject({ phase: 'tracking', intent: { redeemId: '9' } })

    const u = setup()
    await u.flow.start(terms)
    u.flow.receipt(receipt({ logs: [] }))
    expect(u.flow.state.phase).toBe('noEvent')
  })

  it('restores per account and does not show late results in another scope', async () => {
    let release!: (h: string) => void
    const t = setup({ send: () => new Promise((r) => (release = r)) })
    const run = t.flow.start(terms)
    await vi.waitFor(() => expect(t.flow.state.phase).toBe('wallet'))
    const other = { ...scope, user: '0x' + '11'.repeat(20) }
    t.flow.setScope(other)
    expect(t.flow.state).toEqual({ phase: 'idle', intent: null })
    release(HASH)
    await run
    expect(t.flow.state.phase).toBe('idle') // not shown for the other account…
    t.flow.setScope(scope)
    expect(t.flow.state).toMatchObject({ phase: 'confirming', intent: { opId: 'web-1', hash: HASH } }) // …but kept for this one
    expect(t.flow.state.intent?.terms).toBeUndefined()
  })

  it('restores an operation without a hash as unfinished, and New redeem clears it', () => {
    const t = setup()
    t.m.set(`11155111:${CONTRACT}:${USER}`, { opId: 'web-9' })
    t.flow.setScope(null)
    t.flow.setScope(scope)
    expect(t.flow.state.phase).toBe('unfinished')
    t.flow.start(terms)
    expect(t.prepareRedeem).not.toHaveBeenCalled()
    t.flow.reset()
    expect(t.m.size).toBe(0)
    expect(t.flow.state.phase).toBe('idle')
  })
})
