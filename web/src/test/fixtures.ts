import type { BridgeInfo, DepositRecord, Payment, RedeemRecord } from '../api/types'

// Shapes captured from the local backend (GET /v1/info, /v1/deposits/{txid}, /v1/redeems/{id}).
export const info: BridgeInfo = {
  network: 'testnet',
  deposit_address: 'utest1zxyces8xu4c4jqel2m2yw48lxkd9jlzu9ggl6hf5jau0tpyqark4cae0p0u3rc4vng85v3493wngghx0l6aplcpm578867eq7q2xmkct',
  contract: '0x91cac58cea711bd9487721824ea38e2b5742253f',
  chain_id: '11155111',
  zcash_confirmations: 3,
  evm_confirmations: 2,
  fixed_fee_zat: '10000',
  minimum_redeem_zat: '10001',
  scanner_ready: true,
  persisted: { evm_height: 12020174, halted: false, report_current: true, zcash_height: 4554695 },
}

export const minted: DepositRecord = {
  action_index: 0,
  confirmations: 10496,
  deposit_id: '0x35ba18efb92e9f5bc5dea1f248543479b5b38e7f9caa2559bf6dd7b2ec9ed0a0',
  evm_confirmations: 21722,
  mint_transaction: '0x68a8e58dd2e78e113f2a5ff7fa100db065a53ea7e3916db41d120578e563a68a',
  refund: null,
  required_confirmations: 3,
  required_evm_confirmations: 2,
  status: 'minted',
  txid: 'c81e07b82d10c0b2d3e16108c1447b1c2dde2a324299d5b26d792624d1e82d83',
  version: '7',
}

export const deposit = (over: Partial<DepositRecord>): DepositRecord => ({
  ...minted, status: 'awaiting_confirmations', confirmations: 1, evm_confirmations: 0, mint_transaction: null, version: null, ...over,
})

export const payment = (over: Partial<Payment> = {}): Payment => ({
  status: 'paid', confirmations: 10417, required_confirmations: 3, txid: 'c1636ebba7de300936ae0580b32963bbdaa0a6d8cfb4282e79162bdcb152423a',
  inputs_spent: true, internal_change_observed: true, broadcast_acknowledged: true, expiry_height: 4544396, ...over,
})

export const fulfilled: RedeemRecord = {
  id: '1',
  operation_id: 'cwzec/redeem/1',
  status: 'fulfilled',
  version: '3',
  burn_confirmations: 21642,
  required_evm_confirmations: 2,
  payment: payment(),
  fulfillment: { confirmations: 21596, settled: true, transaction: '0xeb2821a3c912dcbe2b39d73833de5c2dbb6b968fc46e97326381905c578289a2' },
  recredit: null,
}

export const redeem = (over: Partial<RedeemRecord>): RedeemRecord => ({ ...fulfilled, ...over })
