import { useEffect, useRef, useState } from 'react'
import type { Hex } from 'viem'
import { DEMO } from '../config'
import type { Wallet } from '../hooks/useWallet'
import { metamask } from '../wallet/metamask'
import { now, outcome, paymentHandle, terms, type Signed } from './core'
import { authorizationData, authorized, clearTracking, configuration, confirmed, loadTracking, recoverAuthorization, retryAuthorization, saveTracking, send, session, type Tracking } from './chain'
import { fetchQuote } from './quote'
import { executeSwap } from './flow'

export function useSwap(wallet: Wallet) {
  const account = wallet.connected ? wallet.address : null
  const [amount, setAmount] = useState('')
  const [slippage, setSlippage] = useState('50')
  const [config, setConfig] = useState<Awaited<ReturnType<typeof configuration>> | null>(null)
  const [quote, setQuote] = useState<Signed | null>(null)
  const [permission, setPermission] = useState(false)
  const [message, setMessage] = useState('')
  const [readError, setReadError] = useState('')
  const [busy, setBusy] = useState(false)
  const [preparingSwap, setPreparingSwap] = useState(false)
  const [tracking, setTracking] = useState<Tracking | null>(null)
  const [completedHash, setCompletedHash] = useState<Hex | null>(null)
  const [handle, setHandle] = useState<Hex | null>(null)
  const [revision, setRevision] = useState(0)
  const [clock, setClock] = useState(now())
  const lifetime = useRef<AbortController | null>(null)
  const generation = useRef(0), lock = useRef(false), scope = useRef(account)
  if (scope.current !== account) { scope.current = account; generation.current++; lifetime.current?.abort() }
  const tracked = (t: Tracking) => { if (scope.current === account) setTracking(t.nonce ? { ...t } : null) }
  useEffect(() => {
    const id = ++generation.current
    let timer: ReturnType<typeof setTimeout>
    const abort = new AbortController()
    lifetime.current = abort
    setAmount(''); setConfig(null); setQuote(null); setPermission(false); setMessage(''); setReadError(''); setHandle(null); setCompletedHash(null)
    setTracking(account ? loadTracking(account) : null)
    const off = metamask.subscribe(() => { generation.current++; setRevision(r => r + 1); abort.abort(); setQuote(null); setHandle(null); setMessage(''); setCompletedHash(null); setConfig(null); setPermission(false) })
    const tick = setInterval(() => setClock(now()), 1000)
    async function poll() {
      if (!account || DEMO) return
      if (lock.current) { timer = setTimeout(poll, 15000); return }
      try {
        await session(account)
        const recovered = await recoverAuthorization(account, () => { if (id !== generation.current || lock.current) throw Error('Operation changed') })
        if (recovered) setTracking(null)
        const c = await configuration()
        const allowed = await authorized(account)
        if (id !== generation.current || lock.current) {
          if (id === generation.current) timer = setTimeout(poll, 15000)
          return
        }
        setConfig(c); setPermission(allowed)
        if (recovered) setMessage(allowed ? 'Operator authorization is active' : 'Previous authorization settled — authorize again')
        const q = await fetchQuote(c.signer, abort.signal)
        if (id === generation.current && !lock.current) { setQuote(q); setReadError('') }
      } catch (e) {
        if (id === generation.current && !lock.current) {
          setQuote(null)
          setReadError((e as { shortMessage?: string }).shortMessage || (e as Error).message)
        }
      }
      if (id === generation.current) timer = setTimeout(poll, 15000)
    }
    void poll()
    return () => { generation.current++; abort.abort(); clearTimeout(timer); clearInterval(tick); off() }
  }, [account, revision])
  let estimate: ReturnType<typeof terms> | null = null
  let invalid = ''
  try { if (quote && config) estimate = terms(amount, quote.quote.priceE8, config.fee, BigInt(slippage)) } catch (e) { invalid = (e as Error).message }
  const priceReady = !!quote && quote.quote.expiresAt > clock
  async function run(work: (check: () => void) => Promise<void>) {
    if (lock.current || !account || DEMO) return
    lock.current = true; setBusy(true); setMessage(''); setReadError('')
    const id = generation.current
    const check = () => { if (id !== generation.current || scope.current !== account) throw Error('Wallet changed — operation stopped') }
    try { await work(check) } catch (e) { if (id === generation.current) setMessage((e as Error).message) }
    finally { lock.current = false; setBusy(false) }
  }
  async function track(t: Tracking, check: () => void) {
    setMessage('Waiting for two confirmations…')
    const result = await confirmed(account!, t, tracked); check(); await session(account!); check()
    clearTracking(account!); setTracking(null); setCompletedHash(result.receipt.transactionHash)
    if (result.replaced) { setMessage('Transaction replaced or cancelled — review wallet activity'); return }
    if (result.receipt.status !== 'success') { setMessage('Transaction reverted'); return }
    if (t.kind === 'authorization') {
      const allowed = await authorized(account!); check(); setPermission(allowed)
      setMessage(allowed ? 'Operator authorization confirmed' : 'Operator authorization expired or inactive — authorize again')
      setQuote(null)
    } else {
      setHandle(paymentHandle(result.receipt.logs, account!))
      setMessage('Transaction confirmed — verify balances')
    }
  }
  const authorize = () => run(async check => {
    setHandle(null)
    setMessage('Checking wallet and Swap contracts…')
    await session(account!); check()
    const c = await configuration(); check(); setConfig(c)
    if (await recoverAuthorization(account!, check)) { setTracking(null) }
    check()
    const previous = loadTracking(account!)
    if (previous?.kind === 'swap') throw Error('Resolve the tracked swap first')
    if (!previous) {
      const allowed = await authorized(account!); check(); setPermission(allowed)
      if (allowed) { setMessage('Operator authorization is already active'); return }
    }
    setMessage(previous ? 'Retry authorization in MetaMask using the previous nonce' : 'Authorize Swap as cwZEC operator for 10 minutes in MetaMask')
    const t = previous
      ? await retryAuthorization(account!, authorizationData(now()), check, tracked)
      : await send(account!, 'authorization', authorizationData(now()), check, tracked)
    check(); await track(t, check)
  })
  const swap = () => {
    const accepted = estimate
    const selectedQuote = quote
    if (!accepted || !selectedQuote || !config || !priceReady) return
    return run(async check => {
      setHandle(null)
      let t: Tracking
      setPreparingSwap(true)
      try {
        t = await executeSwap(account!, accepted, config.fee, check, setMessage, tracked, lifetime.current?.signal)
      } finally { setPreparingSwap(false) }
      check(); await track(t, check)
    })
  }
  const resume = (hash?: string) => run(async check => {
    const t = loadTracking(account!)
    if (!t) { setTracking(null); throw Error('No transaction to track') }
    await session(account!); check()
    if (t.kind === 'authorization' && await recoverAuthorization(account!, check)) {
      setTracking(null)
      const allowed = await authorized(account!); check(); setPermission(allowed)
      setMessage(allowed ? 'Operator authorization is active' : 'Previous authorization settled — authorize again')
      return
    }
    if (hash) {
      if (!/^0x[\da-f]{64}$/i.test(hash)) throw Error('Enter a valid transaction hash')
      t.hash = hash as Hex; saveTracking(account!, t); tracked(t)
    }
    await track(t, check)
  })
  const verify = () => run(async check => {
    if (!handle) return
    await session(account!); check()
    const { decryptPayment } = await import('./fhe'); check()
    try {
      const paid = await decryptPayment(account!, handle, lifetime.current?.signal); check(); await session(account!); check()
      setMessage(outcome(paid)); setHandle(null)
    } catch {
      check(); setMessage('Transaction confirmed — verify balances. Payment reveal unavailable.')
    }
  })
  return { amount, setAmount, slippage, setSlippage, config, quote, priceReady, estimate, invalid, permission, message, readError, busy, preparingSwap, tracking, handle, completedHash, authorize, swap, resume, verify }
}
