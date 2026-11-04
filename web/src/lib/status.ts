import { TunnelError } from '../api/nym'
import type { BridgeInfo, DepositRecord, RedeemRecord } from '../api/types'

export type PanelKind = 'alert' | 'info' | 'check'
export type Stage =
  | { kind: 'progress'; step: number; detail: string }
  | { kind: 'panel'; panel: PanelKind; title: string; detail: string }
  | { kind: 'done' }

const progress = (step: number, detail: string): Stage => ({ kind: 'progress', step, detail })
const panel = (p: PanelKind, title: string, detail: string): Stage => ({ kind: 'panel', panel: p, title, detail })
const unknown = (s: string) => panel('info', 'Unknown status', `The bridge reported "${s}".`)
const of = (n: number | null | undefined, req: number | null | undefined, fallback: number) => `${n ?? 0} of ${req ?? fallback}`

export const DEPOSIT_STEPS = ['Sent', 'Confirming', 'Minting', 'Minted']
export const REDEEM_STEPS = ['Submitted', 'Finalizing', 'Sending ZEC', 'Paid']

/** Maps one backend deposit to the UI. Only `minted` is ever shown as done. */
export function depositStage(d: DepositRecord): Stage {
  switch (d.status) {
    case 'awaiting_confirmations':
      return d.confirmations > 0
        ? progress(1, `${of(d.confirmations, d.required_confirmations, 3)} confirmations`)
        : progress(0, 'Waiting for the network. Usually a few minutes.')
    case 'detected':
      return progress(2, 'Waiting to mint.')
    case 'minting':
      if (!d.mint_transaction) return progress(2, 'Creating your cWZEC.')
      // Enough EVM confirmations is not enough: the encrypted result must also reconcile.
      if ((d.evm_confirmations ?? 0) < (d.required_evm_confirmations ?? 2))
        return progress(2, `${of(d.evm_confirmations, d.required_evm_confirmations, 2)} Sepolia confirmations`)
      return progress(2, 'Verifying mint.')
    case 'minted':
      return { kind: 'done' }
    case 'refund_pending':
      return panel('info', 'Refund pending', "This deposit won't be minted. The operator is returning it.")
    case 'voided':
      return d.refund?.state === 'Paid'
        ? panel('check', 'Refunded', 'The ZEC was returned. No cWZEC was minted.')
        : panel('info', 'Refund in progress', 'No cWZEC was minted. The operator is returning the ZEC.')
    case 'failed':
      return panel('alert', "Can't be minted automatically", 'Contact the operator with this transaction ID. Refunds are manual.')
    default:
      return unknown(d.status)
  }
}

/** Maps a backend redeem. `done` covers paid and fulfilled; the view distinguishes them by status. */
export function redeemStage(r: RedeemRecord): Stage {
  const p = r.payment
  switch (r.status) {
    case 'awaiting_confirmations':
      // The same word is used for the burn (no record yet) and for the Zcash payment.
      return r.version != null && p
        ? progress(2, `${of(p.confirmations, p.required_confirmations, 3)} confirmations`)
        : progress(1, `${of(r.burn_confirmations, r.required_evm_confirmations, 2)} Sepolia confirmations`)
    case 'requested':
      return progress(2, 'Queued for payout.')
    case 'reserved':
    case 'payout_prepared':
      return progress(2, 'Preparing the payment.')
    case 'broadcast':
      return progress(2, 'Payment sent. Waiting for the network.')
    case 'confirmed_observed':
      return progress(2, 'Payment seen. Waiting for settlement.')
    case 'blocked_historical_absence_unproven':
      return panel('info', 'Payout delayed', "The payment didn't go through. The operator will retry it or return your cWZEC.")
    case 'paid':
    case 'fulfilled':
      return { kind: 'done' }
    case 'completed_zero':
      return panel('info', 'Nothing redeemed', 'The amount was more than your balance. No cWZEC was burned and no ZEC will be sent.')
    case 'rejected_before_payout':
      return panel('alert', 'Redeem rejected', "The Zcash address couldn't be used. Your cWZEC was burned and is waiting to be returned by the operator.")
    case 'recredit_pending':
      return panel('info', 'Return pending', 'The burned cWZEC is being returned to your balance.')
    case 're_credited':
      return panel('check', 'Returned to your balance', 'The burned cWZEC was returned. No ZEC was sent.')
    default:
      return unknown(r.status)
  }
}

export type InfoBanner = 'nym' | 'unavail' | 'paused' | 'notready'

/** A failed request wins over stale data; halt is the persisted flag, not a per-direction pause. */
export function infoBanner(info: BridgeInfo | undefined, error: unknown): InfoBanner | null {
  if (error instanceof TunnelError) return 'nym'
  if (error) return 'unavail'
  if (!info) return null
  if (info.persisted?.halted) return 'paused'
  return info.scanner_ready ? null : 'notready'
}
