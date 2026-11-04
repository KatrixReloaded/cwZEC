import { beforeEach, expect, it, vi } from 'vitest'
import { decodeFunctionData } from 'viem'
import { abi, now, POOL, terms } from './core'
const deps = vi.hoisted(() => ({ authorized: vi.fn(), configuration: vi.fn(), send: vi.fn(), session: vi.fn(), fetchQuote: vi.fn(), encrypt: vi.fn() }))
vi.mock('./chain', () => deps)
vi.mock('./quote', () => deps)
vi.mock('./fhe', () => deps)
import { executeSwap } from './flow'
const seller = '0x1111111111111111111111111111111111111111'
const handle = ('0x' + 'aa'.repeat(32)) as `0x${string}`
const accepted = terms('1', 5000000000n, 100n, 50n)
beforeEach(() => {
  vi.resetAllMocks()
  deps.configuration.mockResolvedValue({ fee: 100n, signer: seller }); deps.authorized.mockResolvedValue(true)
  deps.fetchQuote.mockResolvedValue({ quote: { priceE8: 4000000000n, observedAt: now(), expiresAt: now() + 60n }, signature: '0x12' })
  deps.encrypt.mockResolvedValue({ encryptedValues: [handle, handle], inputProof: '0x34' })
  deps.send.mockResolvedValue({ kind: 'swap', nonce: '1', hash: handle })
})
it('uses a fresh quote while preserving the previously accepted minimum when price falls', async () => {
  await executeSwap(seller, accepted, 100n, () => {}, () => {}, () => {})
  expect(deps.encrypt).toHaveBeenCalledWith(seller, 100000000n, 49252500n, undefined)
  const tx = decodeFunctionData({ abi, data: deps.send.mock.calls[0][2] })
  expect(tx.functionName).toBe('swap')
  if (tx.functionName === 'swap') expect(tx.args[4].priceE8).toBe(4000000000n)
  expect(deps.send.mock.calls[0][1]).toBe('swap')
})
it('requires operator permission before encryption and rechecks before sending', async () => {
  deps.authorized.mockResolvedValueOnce(false)
  await expect(executeSwap(seller, accepted, 100n, () => {}, () => {}, () => {})).rejects.toThrow('required')
  expect(deps.encrypt).not.toHaveBeenCalled()
  deps.authorized.mockResolvedValueOnce(true).mockResolvedValueOnce(false)
  await expect(executeSwap(seller, accepted, 100n, () => {}, () => {}, () => {})).rejects.toThrow('expired')
  expect(deps.send).not.toHaveBeenCalled()
})
it('drops a quote response from an old account/network scope', async () => {
  let valid = true
  deps.fetchQuote.mockImplementation(async () => { valid = false; return { quote: { priceE8: 1n, observedAt: now(), expiresAt: now() + 60n } } })
  await expect(executeSwap(seller, accepted, 100n, () => { if (!valid) throw Error('stale account') }, () => {}, () => {})).rejects.toThrow('stale account')
  expect(deps.encrypt).not.toHaveBeenCalled(); expect(deps.send).not.toHaveBeenCalled()
})
it('refuses to send if encryption consumed the quote lifetime', async () => {
  const time = now()
  deps.fetchQuote.mockResolvedValue({ quote: { priceE8: 4000000000n, observedAt: time, expiresAt: time + 1n }, signature: '0x12' })
  deps.encrypt.mockImplementation(async () => { vi.spyOn(Date, 'now').mockReturnValue(Number(time + 2n) * 1000); return { encryptedValues: [handle, handle], inputProof: '0x34' } })
  try { await expect(executeSwap(seller, accepted, 100n, () => {}, () => {}, () => {})).rejects.toThrow('expired') } finally { vi.restoreAllMocks() }
  expect(deps.send).not.toHaveBeenCalled()
})
it('rechecks expiry at the final send boundary and fee before encryption', async () => {
  deps.configuration.mockResolvedValueOnce({ fee: 101n, signer: POOL })
  await expect(executeSwap(seller, accepted, 100n, () => {}, () => {}, () => {})).rejects.toThrow('Fee changed')
  expect(deps.encrypt).not.toHaveBeenCalled()
  await executeSwap(seller, accepted, 100n, () => {}, () => {}, () => {})
  const guard = deps.send.mock.calls[0][3]
  vi.spyOn(Date, 'now').mockReturnValue(Number(now() + 61n) * 1000)
  try { expect(guard).toThrow('expired') } finally { vi.restoreAllMocks() }
})
