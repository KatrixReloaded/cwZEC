import { describe, expect, it } from 'vitest'
import { encodeAbiParameters, encodeEventTopics, type Hex } from 'viem'
import { privateKeyToAccount } from 'viem/accounts'
import { abi, CHAIN, domain, fresh, MAX, outcome, paymentHandle, POOL, terms, types, USDC, validateQuote } from './core'
const signer = privateKeyToAccount(('0x' + '11'.repeat(32)) as Hex)
const quote = { priceE8: 5000000000n, observedAt: 1000n, expiresAt: 1060n }
async function signed(d = domain, q = quote) {
  return { chainId: String(CHAIN), swapAddress: POOL, quote: Object.fromEntries(Object.entries(q).map(([k, v]) => [k, String(v)])), signature: await signer.signTypedData({ domain: d, types, primaryType: 'PriceQuote', message: q }) }
}
describe('Swap exact math', () => {
  it('converts 8 to 6 decimals with one fee rounding and a fixed minimum', () => {
    expect(terms('1', quote.priceE8, 100n, 50n)).toEqual({ amount: 100000000n, output: 49500000n, minimum: 49252500n })
    expect(terms('0.00000003', quote.priceE8, 100n, 0n).output).toBe(1n)
  })
  it('rejects sub-unit, zero, excess precision, negative, exponent and uint64 overflow', () => {
    for (const amount of ['0', '-1', '1e2', '0.000000001', '0.00000001', '184467440737.09551616']) expect(() => terms(amount, quote.priceE8, 100n, 50n)).toThrow()
    expect(() => terms('184467440737.09551615', 100000000000000n, 0n, 0n)).toThrow()
    expect(() => terms('1', quote.priceE8, 1001n, 50n)).toThrow()
  })
})
describe('Swap signed quotes', () => {
  it('accepts the exact Sepolia domain and 60 second lifetime', async () => {
    expect((await validateQuote(await signed(), signer.address, 1000n)).quote).toEqual(quote)
  })
  it('rejects other chain, pool, signer, name, version and tampered prices', async () => {
    const b = await signed()
    for (const body of [{ ...b, chainId: '1' }, { ...b, swapAddress: USDC }, { ...b, quote: { ...b.quote, priceE8: '1' } }, { ...b, signature: '0x12' }, null]) await expect(validateQuote(body, signer.address, 1000n)).rejects.toThrow()
    await expect(validateQuote(b, USDC, 1000n)).rejects.toThrow()
    for (const changes of [{ chainId: 1 }, { verifyingContract: USDC }, { name: 'Other' }, { version: '2' }]) await expect(validateQuote(await signed({ ...domain, ...changes } as typeof domain), signer.address, 1000n)).rejects.toThrow()
  })
  it('rejects at expiry, future observation, overlong lifetime, malformed and out-of-range integers', async () => {
    for (const q of [{ ...quote, observedAt: 1001n }, { ...quote, expiresAt: 1061n }, { ...quote, expiresAt: 1000n }, { ...quote, priceE8: 0n }, { ...quote, priceE8: MAX }, { ...quote, expiresAt: MAX + 1n }]) expect(() => fresh(q, 1000n)).toThrow()
    const b = await signed()
    await expect(validateQuote(b, signer.address, 1060n)).rejects.toThrow()
    for (const v of [1, '-1', '1e3', '0x12', String(MAX + 1n)]) await expect(validateQuote({ ...b, quote: { ...b.quote, priceE8: v } }, signer.address, 1000n)).rejects.toThrow()
  })
})
describe('transaction-specific outcome', () => {
  const handle = ('0x' + 'ab'.repeat(32)) as Hex
  const log = { address: USDC, data: encodeAbiParameters([], []), topics: encodeEventTopics({ abi, eventName: 'ConfidentialTransfer', args: { from: POOL, to: signer.address, amount: handle } }) as Hex[] }
  it('finds only the pinned payment event, including its indexed amount', () => {
    expect(paymentHandle([log], signer.address)).toBe(handle)
    expect(paymentHandle([{ ...log, address: POOL }], signer.address)).toBeNull()
    expect(paymentHandle([log], POOL)).toBeNull()
    expect(paymentHandle([log, log], signer.address)).toBeNull()
  })
  it('distinguishes a confirmed encrypted refund from a filled payment', () => {
    expect(outcome(0n)).toBe('Not filled — cwZEC refunded')
    expect(outcome(49500000n)).toBe('Swapped — received 49.5 cUSDC')
  })
})
