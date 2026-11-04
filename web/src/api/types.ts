// Shapes mirror service/crates/app/src/api/bridge.rs. Zatoshi amounts stay decimal strings.
// Status strings are open-ended: unknown values must be rendered conservatively.

export type Unsubscribe = () => void

export interface BridgeInfo {
  network: string
  deposit_address: string
  contract: string
  chain_id: string
  zcash_confirmations: number
  evm_confirmations: number
  fixed_fee_zat: string
  minimum_redeem_zat: string
  scanner_ready: boolean
  persisted: { halted: boolean; report_current: boolean; zcash_height: number; evm_height: number }
}

export type DepositStatus =
  | 'awaiting_confirmations' | 'detected' | 'minting' | 'minted'
  | 'refund_pending' | 'voided' | 'failed' | (string & {})

export interface Payment {
  status: string
  confirmations: number
  required_confirmations: number
  txid: string | null
  inputs_spent: boolean
  internal_change_observed: boolean
  broadcast_acknowledged: boolean
  expiry_height: number | null
}

export interface DepositRecord {
  txid: string
  action_index: number
  deposit_id: string
  status: DepositStatus
  confirmations: number
  required_confirmations: number
  evm_confirmations: number
  required_evm_confirmations: number
  mint_transaction: string | null
  refund: { state: string; version: string; payment: Payment | null } | null
  version: string | null
}

export type RedeemStatus =
  | 'awaiting_confirmations' | 'requested' | 'reserved' | 'payout_prepared' | 'broadcast'
  | 'confirmed_observed' | 'blocked_historical_absence_unproven' | 'paid' | 'fulfilled'
  | 'completed_zero' | 'rejected_before_payout' | 'recredit_pending' | 're_credited' | (string & {})

export interface ChainTx {
  transaction: string
  confirmations: number
  settled: boolean
}

export interface RedeemRecord {
  id: string
  operation_id: string | null
  status: RedeemStatus
  /** null until the bridge has a canonical record for the burn */
  version: string | null
  burn_confirmations: number
  required_evm_confirmations: number
  payment: Payment | null
  fulfillment: ChainTx | null
  recredit: ChainTx | null
}

export interface PrepareRequest {
  operation_id: string
  user: string
  /** integer zatoshis */
  amount: string
  zcash_address: string
  chain_id: string
  contract: string
}

export interface PreparedTransaction {
  from: string
  to: string
  chainId: string
  value: string
  data: string
}

/** Informational only: the client verifies it before asking the wallet to send anything. */
export interface PrepareResponse {
  operation_id: string
  already_submitted: boolean
  transaction?: PreparedTransaction
  transaction_hash?: string
  fixed_fee_zat?: string
  minimum_redeem_zat?: string
}

export interface BridgeApi {
  getInfo(signal?: AbortSignal): Promise<BridgeInfo>
  /** Empty array = not detected yet. Takes the txid as the wallet displays it. */
  getDeposits(txid: string, signal?: AbortSignal): Promise<DepositRecord[]>
  /** null = not found (yet). */
  getRedeem(id: string, signal?: AbortSignal): Promise<RedeemRecord | null>

  getDepositMemo(recipient: string, signal?: AbortSignal): Promise<string>
  /** Idempotent per operation_id: retries with the same terms return the same calldata. */
  prepareRedeem(req: PrepareRequest, signal?: AbortSignal): Promise<PrepareResponse>
}
