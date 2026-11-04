import { encodeAbiParameters, encodeEventTopics, encodeFunctionData, type Hex } from 'viem'
import type { PrepareResponse } from '../api/types'
import { CWZEC_ABI, type Receipt, type RedeemTerms } from '../lib/redeemTx'

export const USER = '0x12ab34cd56ef7890a1b2c3d4e5f60718293a4b5c'
export const CONTRACT = '0x91cac58cea711bd9487721824ea38e2b5742253f'
export const HASH = ('0x' + 'cd'.repeat(32)) as Hex

export const terms: RedeemTerms = {
  opId: 'web-1', user: USER, contract: CONTRACT, chainId: '11155111', amountZat: '150000000', zaddr: 'utest1' + 'q'.repeat(60),
}

/** euint64 input handle for `chainId`. */
export const handle = (chainId = 11155111n, type = '05') => ('0x' + 'ab'.repeat(22) + chainId.toString(16).padStart(16, '0') + type + '00') as Hex
export const calldata = (h: Hex = handle()) => encodeFunctionData({ abi: CWZEC_ABI, functionName: 'requestRedeem', args: [h, '0x1234', '0xbeef'] })

export const prepared = (over: Partial<PrepareResponse> = {}, tx: Partial<NonNullable<PrepareResponse['transaction']>> = {}): PrepareResponse => ({
  operation_id: terms.opId,
  already_submitted: false,
  transaction: { from: USER, to: CONTRACT, chainId: '0xaa36a7', value: '0x0', data: calldata(), ...tx },
  fixed_fee_zat: '10000',
  minimum_redeem_zat: '10001',
  ...over,
})

export function receipt(over: Partial<Receipt> = {}, event: { id?: bigint; user?: Hex; address?: string } = {}): Receipt {
  const topics = encodeEventTopics({ abi: CWZEC_ABI, eventName: 'RedeemRequested', args: { redeemId: event.id ?? 7n, user: event.user ?? USER } }) as Hex[]
  const data = encodeAbiParameters([{ type: 'bytes32' }, { type: 'bytes' }], [handle(), '0xbeef'])
  return { transactionHash: HASH, status: '0x1', from: USER, logs: [{ address: event.address ?? CONTRACT, topics, data }], ...over }
}
