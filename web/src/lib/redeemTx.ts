import { decodeEventLog, decodeFunctionData, encodeFunctionData, parseAbi, type Hex } from 'viem'
import type { PrepareResponse } from '../api/types'

// CWZEC.requestRedeem / RedeemRequested; euint64 and externalEuint64 are bytes32 in the ABI.
export const CWZEC_ABI = parseAbi([
  'function requestRedeem(bytes32 amount, bytes proof, bytes destCt) returns (uint256)',
  'event RedeemRequested(uint256 indexed redeemId, address indexed user, bytes32 amount, bytes destCt)',
])

/** One deliberate redemption: identity plus the immutable terms it was prepared with. */
export interface RedeemTerms {
  opId: string
  user: string
  contract: string
  /** decimal */
  chainId: string
  /** integer zatoshis */
  amountZat: string
  zaddr: string
}

export interface WalletTx {
  from: Hex
  to: Hex
  chainId: Hex
  value: '0x0'
  data: Hex
}

export interface Receipt {
  transactionHash: string
  status: string
  from: string
  logs: { address: string; topics: Hex[]; data: Hex }[]
}

export class PreparedMismatch extends Error {}

const HASH = /^0x[0-9a-fA-F]{64}$/
const same = (a: unknown, b: string) => typeof a === 'string' && a.toLowerCase() === b.toLowerCase()
const big = (v: unknown) => {
  try {
    return typeof v === 'string' && /^(0x[0-9a-fA-F]+|\d+)$/.test(v) ? BigInt(v) : null
  } catch {
    return null
  }
}

// fhevm handle layout: … | chainId (bytes 22–29) | FHE type (byte 30) | version (byte 31). euint64 = 5.
const handleChain = (h: Hex) => BigInt('0x' + h.slice(46, 62))
const handleType = (h: Hex) => parseInt(h.slice(62, 64), 16)

export function prepareBody(t: RedeemTerms) {
  return { operation_id: t.opId, user: t.user, amount: t.amountZat, zcash_address: t.zaddr, chain_id: t.chainId, contract: t.contract }
}

/**
 * The prepare response is informational. Before anything reaches the wallet, check that it is
 * this operation, for this account, chain and contract, sends no ETH, and is a well-formed
 * `requestRedeem` call whose encrypted amount is a euint64 input for this chain.
 * The encrypted amount and destination themselves cannot be checked in the browser.
 */
export function verifyPrepared(r: PrepareResponse, t: RedeemTerms): { kind: 'send'; tx: WalletTx } | { kind: 'hash'; hash: Hex } {
  const fail = (why: string): never => {
    throw new PreparedMismatch(why)
  }
  if (r?.operation_id !== t.opId) fail('operation')
  if (r.already_submitted === true) {
    if (!HASH.test(r.transaction_hash ?? '')) fail('hash')
    return { kind: 'hash', hash: r.transaction_hash!.toLowerCase() as Hex }
  }
  const tx = r.already_submitted === false ? r.transaction : undefined
  if (!tx) return fail('shape')
  if (!same(tx.from, t.user)) fail('from')
  if (!same(tx.to, t.contract)) fail('to')
  if (big(tx.chainId) !== BigInt(t.chainId)) fail('chain')
  if (big(tx.value) !== 0n) fail('value')
  const min = big(r.minimum_redeem_zat)
  if (min != null && BigInt(t.amountZat) < min) fail('minimum')

  const data = typeof tx.data === 'string' ? (tx.data.toLowerCase() as Hex) : fail('data')
  let args: readonly [Hex, Hex, Hex]
  try {
    const d = decodeFunctionData({ abi: CWZEC_ABI, data })
    args = d.args as readonly [Hex, Hex, Hex]
    // Re-encoding must reproduce the exact bytes: no trailing or non-canonical data.
    if (d.functionName !== 'requestRedeem' || encodeFunctionData({ abi: CWZEC_ABI, functionName: 'requestRedeem', args }) !== data) throw 0
  } catch {
    return fail('calldata')
  }
  const [handle, proof, destCt] = args
  if (/^0x0{64}$/.test(handle) || handleType(handle) !== 5 || handleChain(handle) !== BigInt(t.chainId)) fail('handle')
  if (proof.length <= 2 || destCt.length <= 2) fail('arguments')

  return { kind: 'send', tx: { from: t.user as Hex, to: t.contract as Hex, chainId: `0x${BigInt(t.chainId).toString(16)}`, value: '0x0', data } }
}

/** Reads the redemption ID from a mined receipt; throws if the receipt doesn't prove this burn. */
export function readRedeemReceipt(r: Receipt, hash: string, t: Pick<RedeemTerms, 'user' | 'contract'>): { status: 'reverted' } | { status: 'ok'; redeemId: string } {
  if (!same(r.transactionHash, hash) || !same(r.from, t.user)) throw new Error('receipt')
  if (r.status === '0x0') return { status: 'reverted' }
  if (r.status !== '0x1') throw new Error('status')
  const events = r.logs
    .filter((l) => same(l.address, t.contract))
    .flatMap((l) => {
      try {
        const e = decodeEventLog({ abi: CWZEC_ABI, data: l.data, topics: l.topics as [Hex, ...Hex[]] })
        return e.eventName === 'RedeemRequested' ? [e.args] : []
      } catch {
        return []
      }
    })
  if (events.length !== 1 || !same(events[0].user, t.user)) throw new Error('event')
  return { status: 'ok', redeemId: events[0].redeemId.toString() }
}
