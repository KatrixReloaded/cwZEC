import { useEffect, useRef, useState } from 'react'
import { api } from '../api'
import { formatZat, parseZec } from '../lib/amount'
import { stripSpaces } from '../lib/format'
import { intentStore } from '../lib/intentStore'
import { createRedeemFlow, type FlowState, type Phase } from '../lib/redeemFlow'
import { amountError, isRedeemId, zaddrError } from '../lib/validate'
import { chain } from '../wallet/chain'
import type { Balance } from './useBalance'
import type { BridgeInfoState } from './useBridgeInfo'
import { usePoll } from './usePoll'
import type { Wallet } from './useWallet'

/** Phases where the form shows the intent's immutable terms instead of editable fields. */
const LOCKED = new Set<Phase>(['preparing', 'wallet', 'confirming', 'tracking', 'unknown', 'reverted', 'noEvent'])

export function useRedeem(balance: Balance, bridge: BridgeInfoState, wallet: Wallet) {
  const [amt, setAmt] = useState('')
  const [z, setZ] = useState('')
  const [amtErr, setAmtErr] = useState('')
  const [zErr, setZErr] = useState('')
  const [idInput, setIdInput] = useState('')
  const [idErr, setIdErr] = useState('')
  const [manualId, setManualId] = useState<string | null>(null)
  const [flowState, setFlowState] = useState<FlowState>({ phase: 'idle', intent: null })
  const flow = useRef<ReturnType<typeof createRedeemFlow> | null>(null)
  flow.current ??= createRedeemFlow({ api, chain, store: intentStore }, setFlowState)
  const { phase, intent } = flowState
  const { info, minZat } = bridge
  const known = balance.known

  const scope = wallet.connected && wallet.address && info
    ? { user: wallet.address.toLowerCase(), chainId: info.chain_id, contract: info.contract.toLowerCase() }
    : null
  const scopeKey = scope && `${scope.chainId}:${scope.contract}:${scope.user}`
  useEffect(() => flow.current!.setScope(scope), [scopeKey]) // eslint-disable-line react-hooks/exhaustive-deps

  // Receipt inclusion only; burn confirmations, payout and fulfillment come from the bridge.
  const receipt = usePoll(phase === 'confirming' ? (intent?.hash ?? null) : null, (h, signal) => chain.getReceipt(h, signal))
  useEffect(() => {
    if (receipt.data) flow.current!.receipt(receipt.data)
  }, [receipt.data])

  const flowId = phase === 'tracking' ? (intent?.redeemId ?? null) : null
  const trackedId = flowId ?? manualId
  const status = usePoll(trackedId, (id, signal) => api.getRedeem(id, signal))

  // New redemption work respects the bridge's halt/readiness; tracking continues regardless.
  const blockReason = bridge.halted ? 'Bridge is paused.'
    : !info || bridge.error ? 'Bridge unavailable.'
    : !info.scanner_ready ? "Bridge isn't ready."
    : ''
  const locked = !!intent?.terms && LOCKED.has(phase)
  const edited = () => flow.current!.discardUnsent()

  return {
    amt: locked ? formatZat(intent!.terms!.amountZat) : amt,
    z: locked ? intent!.terms!.zaddr : z,
    amtErr, zErr, idInput, idErr, trackedId, status, phase, intent, receipt, blockReason, locked,
    fromFlow: !!flowId,
    amountZat: locked ? BigInt(intent!.terms!.amountZat) : amtErr ? null : parseZec(amt),

    editAmount(v: string) {
      v = stripSpaces(v)
      setAmt(v)
      edited()
      if (amtErr) setAmtErr(amountError(v, minZat, known))
    },
    blurAmount: () => setAmtErr(amountError(amt, minZat, known)),
    editZ(v: string) {
      v = stripSpaces(v)
      setZ(v)
      edited()
      if (zErr) setZErr(zaddrError(v))
    },
    blurZ: () => setZErr(zaddrError(z)),
    max() {
      if (known == null) return
      setAmt(formatZat(known))
      setAmtErr('')
      edited()
    },
    /** Sets field errors (including "required") and returns whether the form can be submitted. */
    validate() {
      const ae = amountError(amt, minZat, known) || (amt.trim() ? '' : 'Enter an amount.')
      const ze = zaddrError(z) || (z.trim() ? '' : 'Enter a Zcash address.')
      setAmtErr(ae)
      setZErr(ze)
      return !ae && !ze && !blockReason && !flow.current!.busy
    },
    /** Explicit user action only. */
    start() {
      const n = parseZec(amt)
      if (n == null || blockReason) return
      flow.current!.start({ amountZat: n.toString(), zaddr: z.trim() })
    },
    retry: () => !blockReason && flow.current!.retry(),
    reset() {
      flow.current!.reset()
      setAmt('')
      setZ('')
      setAmtErr('')
      setZErr('')
    },

    editId(v: string) {
      setIdInput(stripSpaces(v))
      setIdErr('')
    },
    validateId() {
      const ok = isRedeemId(idInput)
      setIdErr(ok ? '' : 'Enter the redeem ID.')
      return ok
    },
    startTrackId: () => setManualId(idInput),
    stopTracking: () => setManualId(null),
    trackAnother() {
      setManualId(null)
      setIdInput('')
    },
  }
}

export type Redeem = ReturnType<typeof useRedeem>
