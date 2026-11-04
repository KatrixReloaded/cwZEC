import { useEffect, useRef, useState } from 'react'
import { api } from '../api'
import { isEthAddress, isZcashTxId, stripSpaces } from '../lib/format'
import { createLatest } from '../lib/latest'
import { usePoll } from './usePoll'
import type { Wallet } from './useWallet'

export type MemoState = 'empty' | 'loading' | 'ready' | 'failed'

export function useDeposit(wallet: Wallet) {
  const [useWalletAddr, setUseWalletAddr] = useState(true)
  const [recText, setRecText] = useState('')
  const [recErr, setRecErr] = useState('')
  const [memo, setMemo] = useState<{ key: string; value: string | null } | null>(null)
  const [memoTry, setMemoTry] = useState(0)
  const [addrCopied, setAddrCopied] = useState(false)
  const [memoCopied, setMemoCopied] = useState(false)
  const [tx, setTx] = useState('')
  const [txErr, setTxErr] = useState('')
  const [trackedTx, setTrackedTx] = useState<string | null>(null)
  const status = usePoll(trackedTx, (txid, signal) => api.getDeposits(txid, signal))

  const linked = wallet.status === 'connected' || wallet.status === 'wrong'
  const recFromWallet = useWalletAddr && linked
  const recipient = recFromWallet ? wallet.address! : recText
  const recOk = isEthAddress(recipient) && !(recErr && !recFromWallet)
  const memoKey = recOk ? recipient.toLowerCase() : null

  // On connect, prefill from the wallet unless a valid address was already typed; on disconnect, reset.
  useEffect(() => {
    if (wallet.address) setUseWalletAddr(!isEthAddress(recText))
    else {
      setUseWalletAddr(true)
      setRecText('')
      setRecErr('')
      setAddrCopied(false)
    }
  }, [wallet.address]) // eslint-disable-line react-hooks/exhaustive-deps

  // One request per recipient; a changed recipient or account aborts it and ignores its answer.
  const memoLoader = useRef<ReturnType<typeof createLatest<string>> | null>(null)
  memoLoader.current ??= createLatest((key, signal) => api.getDepositMemo(key, signal), (key, r) => setMemo({ key, value: r.ok ? r.value : null }))
  useEffect(() => {
    setMemoCopied(false)
    setMemo(null)
    memoLoader.current!.request(memoKey)
    return memoLoader.current!.cancel
  }, [memoKey, memoTry])

  const current = memo?.key === memoKey ? memo : null
  const memoState: MemoState = !recOk ? 'empty' : !current ? 'loading' : current.value ? 'ready' : 'failed'

  return {
    recFromWallet, recipient, recText, recErr, recOk,
    memo: memoState === 'ready' ? current!.value : null,
    memoState, addrCopied, memoCopied, tx, txErr, trackedTx, status,

    editRecipient(v: string) {
      v = stripSpaces(v)
      setRecText(v)
      setMemoCopied(false)
      if (recErr && (isEthAddress(v) || !v)) setRecErr('')
    },
    blurRecipient() {
      if (!recFromWallet && recText && !isEthAddress(recText)) setRecErr('Enter a valid Ethereum address.')
    },
    useAnother() {
      setUseWalletAddr(false)
      setRecText('')
      setRecErr('')
    },
    useConnected() {
      setUseWalletAddr(true)
      setRecErr('')
    },
    retryMemo: () => setMemoTry((n) => n + 1),
    markAddrCopied: () => setAddrCopied(true),
    markMemoCopied: () => setMemoCopied(true),

    editTx(v: string) {
      setTx(stripSpaces(v))
      setTxErr('')
    },
    /** Public lookup: only the ID format is checked. Sets the error and returns false if invalid. */
    validateTrack() {
      const ok = isZcashTxId(tx)
      setTxErr(ok ? '' : 'Enter the 64-character transaction ID.')
      return ok
    },
    // Passed through as displayed by the wallet; the backend handles byte order.
    startTrack: () => setTrackedTx(tx.toLowerCase()),
    changeTx: () => setTrackedTx(null),
    trackAnother() {
      setTrackedTx(null)
      setTx('')
      setTxErr('')
    },
  }
}

export type Deposit = ReturnType<typeof useDeposit>
