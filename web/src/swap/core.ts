import { decodeEventLog, parseAbi, recoverTypedDataAddress, type Hex } from 'viem'
import { parseZec } from '../lib/amount'
export const CHAIN = 11155111
export const POOL = '0xa47a8e065836082061f58d2a2c751f72f1226426' as const
export const ZEC = '0x91cac58cea711bd9487721824ea38e2b5742253f' as const
export const USDC = '0xd2bbe2681a3cb05c73901dcc836f676801d013a3' as const
export const UNDERLYING = '0x12445a9dd878282004e1e76d6124a5d191fdbecb' as const
export const BALANCER = '0x89f2d06a3cd63b6c0a2ec14c341b3f8b02ce963c' as const
export const SIGNER = '0xf988419246ab423a3c78075bf1dafb6be087c6f8' as const
export const MAX = (1n << 64n) - 1n
export const abi = parseAbi([
  'function cwzec() view returns (address)', 'function cusdc() view returns (address)',
  'function balancer() view returns (address)', 'function quoteSigner() view returns (address)',
  'function feeBps() view returns (uint16)', 'function decimals() view returns (uint8)',
  'function underlying() view returns (address)', 'function token() view returns (address)',
  'function isOperator(address holder,address spender) view returns (bool)',
  'function setOperator(address operator,uint48 until)',
  'function swap(bytes32 amount,bytes32 minOut,bytes proof,uint256 deadline,(uint64 priceE8,uint64 observedAt,uint64 expiresAt) quote,bytes signature)',
  'event ConfidentialTransfer(address indexed from,address indexed to,bytes32 indexed amount)',
])
export const domain = { name: 'CWZECSwap', version: '1', chainId: CHAIN, verifyingContract: POOL } as const
export const types = { PriceQuote: [{ name: 'priceE8', type: 'uint64' }, { name: 'observedAt', type: 'uint64' }, { name: 'expiresAt', type: 'uint64' }] } as const
export type Quote = { priceE8: bigint; observedAt: bigint; expiresAt: bigint }
export type Signed = { quote: Quote; signature: Hex }
export const now = () => BigInt(Math.floor(Date.now() / 1000))
export function fresh(q: Quote, time = now()) {
  if (q.priceE8 <= 0n || q.priceE8 > 100_000_000_000_000n || q.observedAt < 0n || q.expiresAt > MAX || q.observedAt > time || q.expiresAt <= time || q.expiresAt <= q.observedAt || q.expiresAt - q.observedAt > 60n) throw Error('Price unavailable — invalid or expired quote')
}
export async function validateQuote(body: unknown, signer: string, time = now()): Promise<Signed> {
  const b = body as Record<string, unknown> | null
  if (!b || b.chainId !== String(CHAIN) || typeof b.swapAddress !== 'string' || b.swapAddress.toLowerCase() !== POOL || typeof b.signature !== 'string' || !/^0x[0-9a-fA-F]{130}$/.test(b.signature)) throw Error('Price unavailable — invalid response')
  const raw = b.quote as Record<string, unknown> | null
  const integer = (v: unknown) => { if (typeof v !== 'string' || !/^\d{1,20}$/.test(v) || BigInt(v) > MAX) throw Error('Invalid quote integer'); return BigInt(v) }
  const quote = { priceE8: integer(raw?.priceE8), observedAt: integer(raw?.observedAt), expiresAt: integer(raw?.expiresAt) }
  fresh(quote, time)
  const signature = b.signature as Hex
  if ((await recoverTypedDataAddress({ domain, types, primaryType: 'PriceQuote', message: quote, signature })).toLowerCase() !== signer.toLowerCase()) throw Error('Price unavailable — wrong signer')
  fresh(quote, time)
  return { quote, signature }
}
export function terms(text: string, price: bigint, fee: bigint, slippage: bigint) {
  const amount = text.length <= 40 ? parseZec(text) : null
  if (!amount || amount > MAX || price <= 0n || price > 100_000_000_000_000n || fee < 0n || fee > 1000n || slippage < 0n || slippage > 500n) throw Error('Enter a valid amount (up to 8 decimals)')
  const output = amount * price * (10000n - fee) / 100_000_000_000_000n
  const minimum = output * (10000n - slippage) / 10000n
  if (!minimum || output > MAX) throw Error('Output is outside uint64 range or below one unit')
  return { amount, output, minimum }
}
export const units = (v: bigint, decimals = 6) => `${v / 10n ** BigInt(decimals)}.${(v % 10n ** BigInt(decimals)).toString().padStart(decimals, '0').replace(/0+$/, '') || '0'}`
export function paymentHandle(logs: readonly { address: string; data: Hex; topics: readonly Hex[] }[], seller: string): Hex | null {
  const matches: Hex[] = []
  for (const log of logs) {
    if (log.address.toLowerCase() !== USDC) continue
    try {
      const e = decodeEventLog({ abi, data: log.data, topics: log.topics as [Hex, ...Hex[]] })
      if (e.eventName === 'ConfidentialTransfer' && e.args.from.toLowerCase() === POOL && e.args.to.toLowerCase() === seller.toLowerCase()) matches.push(e.args.amount)
    } catch { /* unrelated event */ }
  }
  return matches.length === 1 ? matches[0] : null
}
export const outcome = (payment: bigint) => payment === 0n ? 'Not filled — cwZEC refunded' : `Swapped — received ${units(payment)} cUSDC`
