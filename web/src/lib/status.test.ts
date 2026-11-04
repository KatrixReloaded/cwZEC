import { describe, expect, it } from 'vitest'
import { deposit, fulfilled, info, minted, payment, redeem } from '../test/fixtures'
import { depositStage, infoBanner, redeemStage } from './status'

describe('depositStage', () => {
  it('maps confirmations', () => {
    expect(depositStage(deposit({ confirmations: 0 }))).toEqual({ kind: 'progress', step: 0, detail: 'Waiting for the network. Usually a few minutes.' })
    expect(depositStage(deposit({ confirmations: 2 }))).toEqual({ kind: 'progress', step: 1, detail: '2 of 3 confirmations' })
    expect(depositStage(deposit({ status: 'detected', confirmations: 3 }))).toMatchObject({ kind: 'progress', step: 2 })
  })

  it('stays minting until the backend says minted, even with enough EVM confirmations', () => {
    const tx = minted.mint_transaction
    expect(depositStage(deposit({ status: 'minting' }))).toEqual({ kind: 'progress', step: 2, detail: 'Creating your cWZEC.' })
    expect(depositStage(deposit({ status: 'minting', mint_transaction: tx, evm_confirmations: 1 }))).toEqual({ kind: 'progress', step: 2, detail: '1 of 2 Sepolia confirmations' })
    expect(depositStage(deposit({ status: 'minting', mint_transaction: tx, evm_confirmations: 5 }))).toEqual({ kind: 'progress', step: 2, detail: 'Verifying mint.' })
    expect(depositStage(minted)).toEqual({ kind: 'done' })
  })

  it('maps refund and failure states', () => {
    expect(depositStage(deposit({ status: 'refund_pending' }))).toMatchObject({ kind: 'panel', title: 'Refund pending' })
    expect(depositStage(deposit({ status: 'voided' }))).toMatchObject({ kind: 'panel', title: 'Refund in progress' })
    expect(depositStage(deposit({ status: 'voided', refund: { state: 'Paid', version: '2', payment: payment() } }))).toMatchObject({ panel: 'check', title: 'Refunded' })
    expect(depositStage(deposit({ status: 'failed' }))).toMatchObject({ panel: 'alert', title: "Can't be minted automatically" })
  })

  it('treats unknown statuses conservatively and tolerates null fields', () => {
    expect(depositStage(deposit({ status: 'teleported' }))).toMatchObject({ kind: 'panel', title: 'Unknown status' })
    const nulls = deposit({ status: 'minting', mint_transaction: '0xabc', evm_confirmations: null as never, required_evm_confirmations: null as never })
    expect(depositStage(nulls)).toEqual({ kind: 'progress', step: 2, detail: '0 of 2 Sepolia confirmations' })
  })

  it('follows confirmations backwards after a reorg', () => {
    expect(depositStage(deposit({ confirmations: 2 }))).toMatchObject({ detail: '2 of 3 confirmations' })
    expect(depositStage(deposit({ confirmations: 1 }))).toMatchObject({ detail: '1 of 3 confirmations' })
  })
})

describe('redeemStage', () => {
  const pending = { version: null, operation_id: null, payment: null, fulfillment: null }

  it('distinguishes burn confirmations from payment confirmations', () => {
    expect(redeemStage(redeem({ ...pending, status: 'awaiting_confirmations', burn_confirmations: 1 }))).toEqual({ kind: 'progress', step: 1, detail: '1 of 2 Sepolia confirmations' })
    expect(redeemStage(redeem({ status: 'awaiting_confirmations', payment: payment({ status: 'awaiting_confirmations', confirmations: 2 }) }))).toEqual({ kind: 'progress', step: 2, detail: '2 of 3 confirmations' })
  })

  it('keeps observed payment, paid, fulfilled and re-credited distinct', () => {
    expect(redeemStage(redeem({ status: 'confirmed_observed' }))).toMatchObject({ kind: 'progress', step: 2 })
    expect(redeemStage(redeem({ status: 'paid', fulfillment: null }))).toEqual({ kind: 'done' })
    expect(redeemStage(fulfilled)).toEqual({ kind: 'done' })
    expect(redeemStage(redeem({ status: 'recredit_pending' }))).toMatchObject({ panel: 'info', title: 'Return pending' })
    expect(redeemStage(redeem({ status: 're_credited' }))).toMatchObject({ panel: 'check', title: 'Returned to your balance' })
  })

  it('maps terminal and blocked states', () => {
    expect(redeemStage(redeem({ status: 'completed_zero' }))).toMatchObject({ title: 'Nothing redeemed' })
    expect(redeemStage(redeem({ status: 'rejected_before_payout' }))).toMatchObject({ panel: 'alert', title: 'Redeem rejected' })
    expect(redeemStage(redeem({ status: 'blocked_historical_absence_unproven' }))).toMatchObject({ title: 'Payout delayed' })
    for (const s of ['requested', 'reserved', 'payout_prepared', 'broadcast']) expect(redeemStage(redeem({ status: s }))).toMatchObject({ kind: 'progress', step: 2 })
    expect(redeemStage(redeem({ status: 'mystery' }))).toMatchObject({ title: 'Unknown status' })
  })
})

describe('infoBanner', () => {
  it('reports loading, failure, halt and readiness accurately', () => {
    expect(infoBanner(undefined, undefined)).toBeNull()
    expect(infoBanner(info, undefined)).toBeNull()
    expect(infoBanner(info, new Error('x'))).toBe('unavail') // stale data does not hide a failure
    expect(infoBanner({ ...info, scanner_ready: false }, undefined)).toBe('notready')
    expect(infoBanner({ ...info, scanner_ready: false, persisted: { ...info.persisted, halted: true } }, undefined)).toBe('paused')
  })
})
