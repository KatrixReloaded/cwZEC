// Demo mode only (VITE_BRIDGE_MODE=demo): simulated progress, never a real bridge result.
import { encodeFunctionData, type Hex } from 'viem'
import { CWZEC_ABI } from '../lib/redeemTx'
import type { BridgeApi, BridgeInfo, DepositRecord, PrepareResponse, RedeemRecord } from './types'

const wait = (ms: number) => new Promise((r) => setTimeout(r, ms))
const FEE = 10000n
let balance = 1_250_000_000n
const firstSeen = new Map<string, number>()
const submitted = new Map<string, { at: number; zero: boolean }>()
const prepared = new Map<string, { terms: string; amount: bigint; res: PrepareResponse }>()
let lastAmount = 0n

export const demoBalance = () => balance
/** Called by the demo chain once its simulated burn is "mined". */
export function demoRedeemSubmitted(id: string) {
  if (submitted.has(id)) return
  const zero = lastAmount > balance
  if (!zero) balance -= lastAmount
  submitted.set(id, { at: Date.now(), zero })
}

const elapsed = (key: string) => {
  if (!firstSeen.has(key)) firstSeen.set(key, Date.now())
  return Date.now() - firstSeen.get(key)!
}

const INFO: BridgeInfo = {
  network: 'testnet',
  deposit_address: 'utest1epvvulhx55ezsut67c5efxl2a8m676qeucp5t7f2hf89qrn4c0vcwdkt9sn5ryxk48sa68rc2tt93l7yyc4n74v2ltd8sn9v6y7d952jskd2zk8l4l564uj83570qt98jf4l6a8fecmfavhqusfsrc8dharhdwf6unyjswgcq6rtepk6',
  contract: '0x0000000000000000000000000000000000000de0',
  chain_id: '11155111',
  zcash_confirmations: 3,
  evm_confirmations: 2,
  fixed_fee_zat: FEE.toString(),
  minimum_redeem_zat: (FEE + 1n).toString(),
  scanner_ready: true,
  persisted: { halted: false, report_current: true, zcash_height: 0, evm_height: 0 },
}

function deposit(txid: string, action_index: number, t: number): DepositRecord {
  const base = {
    txid, action_index, deposit_id: '0x' + action_index.toString(16).padStart(64, '0'), refund: null,
    required_confirmations: 3, required_evm_confirmations: 2, version: null, mint_transaction: null,
    confirmations: Math.min(3, 1 + Math.floor(t / 1200)), evm_confirmations: 0,
  }
  const mint = '0x8b1e2f7a0c93d5e14f6b28a9c07e3d51b2f8a64c19e0d7b35a2c6f9e81d044c2'
  if (t < 3600) return { ...base, status: 'awaiting_confirmations' }
  if (t < 4800) return { ...base, status: 'detected' }
  if (t < 6000) return { ...base, status: 'minting', version: '1' }
  if (t < 8400) return { ...base, status: 'minting', version: '1', mint_transaction: mint, evm_confirmations: t < 7200 ? 1 : 2 }
  return { ...base, status: 'minted', version: '2', mint_transaction: mint, evm_confirmations: 3 }
}

function redeem(id: string, t: number, zero: boolean): RedeemRecord {
  const base = { id, operation_id: null, version: null, burn_confirmations: Math.min(2, 1 + Math.floor(t / 1300)), required_evm_confirmations: 2, payment: null, fulfillment: null, recredit: null }
  if (t < 2600) return { ...base, status: 'awaiting_confirmations' }
  const rec = { ...base, operation_id: `cwzec/redeem/${id}`, version: '1' }
  if (zero) return { ...rec, status: 'completed_zero' }
  if (t < 3600) return { ...base, status: 'requested' }
  const pay = (status: string, confirmations: number) => ({
    status, confirmations, required_confirmations: 3, txid: 'c315da22d1464e95c9a1a0b9384880562db941492fd4339f867712708aa4dbcd',
    inputs_spent: confirmations > 0, internal_change_observed: confirmations > 0, broadcast_acknowledged: true, expiry_height: null,
  })
  if (t < 4600) return { ...rec, status: 'broadcast', payment: pay('broadcast', 0) }
  if (t < 8200) {
    const c = 1 + Math.floor((t - 4600) / 1200)
    return { ...rec, status: 'awaiting_confirmations', payment: pay('awaiting_confirmations', c) }
  }
  if (t < 9400) return { ...rec, status: 'confirmed_observed', payment: pay('confirmed_observed', 3) }
  const fulfillment = { transaction: '0x5c07d1b4e8a2f36c90d17e5ba43f2c8d61e09a7b5f3c24d8e16a90b7c2f59e3a', confirmations: 1, settled: false }
  if (t < 11000) return { ...rec, status: 'paid', payment: pay('paid', 4), fulfillment }
  return { ...rec, status: 'fulfilled', version: '3', payment: pay('paid', 5), fulfillment: { ...fulfillment, confirmations: 3, settled: true } }
}

export const mockApi: BridgeApi = {
  async getInfo() {
    return INFO
  },
  /** Any txid starts "not detected"; one starting with 00 carries two deposits. */
  async getDeposits(txid) {
    const t = elapsed(txid) - 2000
    if (t < 0) return []
    return txid.startsWith('00') ? [deposit(txid, 0, t), deposit(txid, 1, t - 1500)] : [deposit(txid, 0, t)]
  },
  async getRedeem(id) {
    const s = submitted.get(id)
    return s ? redeem(id, Date.now() - s.at, s.zero) : null
  },
  async getDepositMemo(recipient) {
    await wait(400)
    return `cwzec:v1:${INFO.chain_id}:${recipient.toLowerCase()}:demo0000`
  },
  async prepareRedeem(req) {
    await wait(900)
    const terms = JSON.stringify([req.user.toLowerCase(), req.amount, req.zcash_address])
    const old = prepared.get(req.operation_id)
    if (old && old.terms !== terms) throw new Error('terms changed')
    if (old) return (lastAmount = old.amount), old.res
    // euint64 input handle for Sepolia: chain ID in bytes 22–29, type 5, version 0.
    const handle = ('0x' + 'ab'.repeat(22) + (11155111).toString(16).padStart(16, '0') + '0500') as Hex
    const data = encodeFunctionData({ abi: CWZEC_ABI, functionName: 'requestRedeem', args: [handle, '0x01', '0x02'] })
    const res: PrepareResponse = {
      operation_id: req.operation_id, already_submitted: false,
      transaction: { from: req.user, to: INFO.contract, chainId: '0xaa36a7', value: '0x0', data },
      fixed_fee_zat: INFO.fixed_fee_zat, minimum_redeem_zat: INFO.minimum_redeem_zat,
    }
    prepared.set(req.operation_id, { terms, amount: BigInt(req.amount), res })
    lastAmount = BigInt(req.amount)
    return res
  },
}
