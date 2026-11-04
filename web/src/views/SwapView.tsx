import { useEffect, useState } from 'react'
import { DEMO, explorerTx } from '../config'
import { Section } from '../components/ui'
import { usePrivateBalance } from '../swap/usePrivateBalance'
import type { Wallet } from '../hooks/useWallet'
import { CHAIN, units, USDC, ZEC } from '../swap/core'
import { useSwap } from '../swap/useSwap'
import '../swap/swap.css'

export function SwapView({ wallet, onConnect }: { wallet: Wallet; onConnect: () => void }) {
  const s = useSwap(wallet)
  const [recovery, setRecovery] = useState('')
  const scope = wallet.connected && wallet.address && !DEMO ? { account: wallet.address, chainId: String(CHAIN) } : null
  const z = usePrivateBalance(scope ? { ...scope, contract: ZEC } : null)
  const u = usePrivateBalance(scope ? { ...scope, contract: USDC } : null)
  useEffect(() => { z.hide(); u.hide() }, [s.completedHash, s.tracking?.hash])
  return <div className="cw-grid swap-page">
    <div className="cw-col">
      <Section i={0}>
        <div className="swap-pair"><h2>cwZEC → cUSDC</h2><span className="muted">Sepolia · Mock-backed cUSDC</span></div>
        <div className="fld">
          <label className="lbl" htmlFor="swap-amount">You pay · cwZEC</label>
          <div className="hud"><input id="swap-amount" className="inp mono" inputMode="decimal" autoComplete="off" placeholder="0.0" maxLength={40} value={s.amount} disabled={s.preparingSwap} onChange={e => s.setAmount(e.target.value)} aria-describedby="swap-validation" aria-invalid={!!s.amount && !!s.invalid} /></div>
          <div className="help" id="swap-validation">{s.amount && s.invalid ? s.invalid : 'Up to 8 decimals'}</div>
        </div>
      </Section>
      <Section i={1}>
        <dl className="dl">
          <div className="dl-row"><dt>Estimated receive</dt><dd>{s.priceReady && s.estimate ? `${units(s.estimate.output)} cUSDC` : '—'}</dd></div>
          <div className="dl-row"><dt>Minimum receive</dt><dd>{s.priceReady && s.estimate ? `${units(s.estimate.minimum)} cUSDC` : '—'}</dd></div>
          <div className="dl-row"><dt>Fee</dt><dd>{s.config ? `${units(s.config.fee, 2)}%` : '—'}</dd></div>
        </dl>
        <div className="swap-slippage"><label htmlFor="swap-slippage">Slippage</label><select id="swap-slippage" value={s.slippage} disabled={s.preparingSwap} onChange={e => s.setSlippage(e.target.value)}><option value="10">0.1%</option><option value="50">0.5%</option><option value="100">1%</option></select></div>
        <span className="help">{s.priceReady ? 'Signed reference price · inventory is confidential' : 'Price unavailable'}</span>
      </Section>
      <Section i={2}>
        {DEMO ? <span className="help">Swap requires live mode. No simulated swaps.</span> : !wallet.connected ? <button className="btn btn-p btn-x" onClick={wallet.status === 'wrong' ? wallet.switchNetwork : onConnect}>{wallet.status === 'wrong' ? 'Switch to Sepolia' : 'Connect wallet'}</button> : <>
          {(!s.permission || s.tracking?.kind === 'authorization') && <><button className="btn btn-s" disabled={s.busy || s.tracking?.kind === 'swap'} onClick={() => void s.authorize()}>{s.busy ? 'Working…' : s.tracking?.kind === 'authorization' ? 'Retry authorization' : 'Authorize operator · 10 min'}</button><span className="help">{s.tracking?.kind === 'authorization' ? 'Reuses the previous nonce; may replace the earlier authorization.' : 'Allows this pool to transfer cwZEC for 10 minutes.'}</span></>}
          <button className="btn btn-p btn-x" disabled={s.busy || !!s.tracking || !s.priceReady || !s.estimate || !s.permission} onClick={() => void s.swap()}>Swap</button>
        </>}
        <div role="status" aria-live="polite" className="swap-status">{s.message || s.readError || (s.busy ? 'Working…' : 'Encrypted amounts. Confirm each request in MetaMask.')}</div>
      </Section>
    </div>
    <div className="cw-col">
      <Section i={1} m={3}>
        <span className="lbl">Private wallet balances</span>
        {([['cwZEC', z, 8], ['cUSDC', u, 6]] as const).map(([name, b, decimals]) => <div className="swap-balance" key={name}>
          <span>{name} · {wallet.connected && b.known !== null ? units(b.known, decimals) : 'Encrypted'}</span>
          <button className="btn btn-t" disabled={!b.canReveal || b.state === 'signing' || b.state === 'decrypting'} onClick={b.known !== null ? b.hide : b.reveal}>{b.known !== null ? 'Hide' : b.state === 'signing' || b.state === 'decrypting' ? 'Revealing…' : 'Reveal'}</button>
          {(b.state === 'failed' || b.state === 'rejected') && <span className="help">Reveal unavailable. Try again.</span>}
        </div>)}
      </Section>
      <Section i={2} m={4}>
        {(s.tracking?.hash || s.completedHash) && <a href={explorerTx(s.tracking?.hash ?? s.completedHash!)} target="_blank" rel="noreferrer">View {s.tracking?.kind ?? 'swap'} transaction ↗</a>}
        {s.tracking && <>
          {!s.tracking.hash && <><span className="help">{s.tracking.kind === 'authorization' ? 'No transaction hash saved. Check MetaMask activity or retry authorization above.' : 'Outcome uncertain. Check MetaMask activity; do not submit again.'}</span><label className="lbl" htmlFor="swap-recovery">Transaction hash</label><input className="inp mono" id="swap-recovery" value={recovery} onChange={e => setRecovery(e.target.value)} /></>}
          <button className="btn btn-s" disabled={s.busy} onClick={() => void s.resume(recovery || undefined)}>Check confirmations</button>
        </>}
        {s.handle && <button className="btn btn-s" disabled={s.busy} onClick={() => void s.verify()}>Verify exact payment · sign reveal</button>}
        <span className="help">A confirmed transaction can refund cwZEC and pay zero cUSDC. Verify payment or reveal balances.</span>
      </Section>
    </div>
  </div>
}
