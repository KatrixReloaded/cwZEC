import type { ReactNode } from 'react'
import type { RedeemRecord } from '../api'
import type { Phase } from '../lib/redeemFlow'
import type { PanelKind } from '../lib/status'
import { explorerTx } from '../config'
import type { Balance } from '../hooks/useBalance'
import type { BridgeInfoState } from '../hooks/useBridgeInfo'
import type { Deposit } from '../hooks/useDeposit'
import { useFx } from '../hooks/useFx'
import type { Redeem } from '../hooks/useRedeem'
import type { Wallet } from '../hooks/useWallet'
import { formatZat } from '../lib/amount'
import { shortHex } from '../lib/format'
import { DEPOSIT_STEPS, REDEEM_STEPS, depositStage, redeemStage } from '../lib/status'
import { amountError, zaddrError } from '../lib/validate'
import { BalanceRow } from '../components/BalanceRow'
import { Icon, Spinner } from '../components/Icon'
import { CopyTextButton, ErrorMsg, ExplorerLink, Panel, PollNotice, Progress, Section } from '../components/ui'

type Props = { r: Redeem; wallet: Wallet; balance: Balance; deposit: Deposit; bridge: BridgeInfoState; onConnect: () => void }

type Act = 'retry' | 'new'
const PANELS: Partial<Record<Phase, [PanelKind, string, string, Act]>> = {
  prepareFailed: ['alert', "Couldn't prepare the redeem.", 'Check the amount and Zcash address.', 'retry'],
  invalid: ['alert', "Prepared transaction didn't match.", 'Nothing was sent.', 'new'],
  cancelled: ['alert', 'Transaction cancelled in wallet.', '', 'retry'],
  mismatch: ['alert', 'Wallet changed.', 'Use the account that started this redeem, on Sepolia.', 'retry'],
  unknown: ['info', 'Submission unknown', "Check your wallet's activity before trying again.", 'new'],
  reverted: ['alert', 'Transaction failed on Sepolia. Nothing was burned.', '', 'new'],
  noEvent: ['alert', "Couldn't read the redeem ID.", 'Check the transaction on Sepolia.', 'new'],
  unfinished: ['info', 'Unfinished redeem', "If your wallet sent it, check its activity before trying again.", 'new'],
}

export function RedeemView({ r, wallet, balance, deposit, bridge, onConnect }: Props) {
  const redeemFx = useFx()
  const connectFx = useFx()
  const { feeZat, minZat } = bridge
  const { phase, locked } = r
  const n = r.amountZat
  const amtOk = n != null && n > 0n && (locked || !amountError(r.amt, minZat, balance.known))
  const zOk = !!r.z && !r.zErr && !zaddrError(r.z)
  const hasErr = !!(r.amtErr || r.zErr)
  const revealed = wallet.connected && balance.known != null
  const submit = () => (r.validate() ? redeemFx.burst(r.start) : redeemFx.shake())
  const reason = r.blockReason || (hasErr ? 'Fix the errors above.' : '')
  const panel = PANELS[phase]
  const hash = r.intent?.hash
  const tracked = r.trackedId != null

  let action: ReactNode = null
  if (phase !== 'idle') {
    action = (
      <>
        {phase === 'preparing' && <button type="button" className="btn btn-s btn-full busy" aria-disabled="true"><Spinner /><span>Preparing…</span></button>}
        {phase === 'wallet' && (
          <div className="fld">
            <button type="button" className="btn btn-s btn-full busy" aria-disabled="true" aria-describedby="r-wallet-h"><Icon name="wallet" /><span>Confirm in wallet</span></button>
            <div className="help" id="r-wallet-h">Check your wallet to continue.</div>
          </div>
        )}
        {phase === 'confirming' && <Progress label="Redeem progress" steps={REDEEM_STEPS} current={0} detail="Waiting for Sepolia." />}
        {panel && (
          <Panel kind={panel[0]} title={panel[1]} detail={panel[2] || undefined}>
            {panel[3] === 'retry'
              ? <button type="button" className="btn btn-p" disabled={!!r.blockReason} onClick={r.retry}>{phase === 'cancelled' ? 'Redeem' : 'Retry'}</button>
              : <button type="button" className="btn btn-s" onClick={r.reset}>New redeem</button>}
          </Panel>
        )}
        {panel?.[3] === 'retry' && r.blockReason && <div className="reason"><Icon name="info" /><span>{r.blockReason}</span></div>}
        {hash && (
          <dl className="dl">
            <div className="dl-row"><dt>Redeem transaction</dt><dd><ExplorerLink href={explorerTx(hash)} hash={shortHex(hash)} what="Redeem transaction" /></dd></div>
          </dl>
        )}
        {phase === 'confirming' && r.receipt.stale && <div className="help">Couldn't refresh. Showing last known status.</div>}
      </>
    )
  } else if (wallet.status === 'none') {
    action = <button type="button" className={`btn btn-p btn-full btn-x ${connectFx.cls}`} onClick={() => (connectFx.burst(), onConnect())}><Icon name="wallet" /><span>Connect wallet</span></button>
  } else if (wallet.status === 'connecting') {
    action = <button type="button" className="btn btn-s btn-full busy" aria-disabled="true"><Spinner /><span>Connecting…</span></button>
  } else if (wallet.status === 'wrong') {
    action = <button type="button" className="btn btn-p btn-full" onClick={wallet.switchNetwork}>Switch to Sepolia</button>
  } else if (reason) {
    action = (
      <div>
        <button type="button" className="btn btn-p btn-full" disabled aria-describedby="r-reason">Redeem</button>
        <div className="reason" id="r-reason"><Icon name="info" /><span>{reason}</span></div>
      </div>
    )
  } else {
    action = <button type="button" className={`btn btn-p btn-full btn-x ${redeemFx.cls}`} onClick={submit}>Redeem</button>
  }

  return (
    <div className="cw-grid">
      <div className="cw-col">
        <Section i={0}>
          <div className="fld">
            <span className="lbl">Balance</span>
            <BalanceRow balance={balance} connected={wallet.connected} />
          </div>
        </Section>

        <Section i={1}>
          <div className="fld">
            <div className="lbl-row">
              <label className="lbl" htmlFor="r-amt">Amount</label>
              {!locked && revealed && <button type="button" className="btn btn-t" onClick={r.max}>Max</button>}
            </div>
            {locked ? (
              <output id="r-amt" className="ro-box">{(amtOk ? formatZat(n!) : r.amt) + ' cWZEC'}</output>
            ) : (
              <div className="inp-wrap">
                <input
                  id="r-amt" className={`inp mono ${r.amtErr ? 'err' : ''}`} type="text" inputMode="decimal" autoComplete="off" spellCheck={false}
                  aria-invalid={!!r.amtErr} aria-describedby="r-amt-m"
                  value={r.amt} onChange={(e) => r.editAmount(e.target.value)} onBlur={r.blurAmount}
                />
                <span className="sfx" aria-hidden="true">cWZEC</span>
              </div>
            )}
            {r.amtErr && <ErrorMsg id="r-amt-m">{r.amtErr}</ErrorMsg>}
            {!locked && !r.amtErr && !revealed && <div className="help" id="r-amt-m">Amounts above your balance redeem nothing.</div>}
          </div>
        </Section>

        <Section i={2}>
          <div className="fld">
            <label className="lbl" htmlFor="r-z">Zcash address</label>
            {locked ? (
              <output id="r-z" className="ro-box">{r.z}</output>
            ) : (
              <div className="hud">
                <textarea
                  id="r-z" className={`inp mono ta two ${r.zErr ? 'err' : ''}`} rows={2} spellCheck={false} autoComplete="off"
                  aria-invalid={!!r.zErr} aria-describedby="r-z-m r-z-h"
                  value={r.z} onChange={(e) => r.editZ(e.target.value)} onBlur={r.blurZ}
                />
              </div>
            )}
            {r.zErr && <ErrorMsg id="r-z-m">{r.zErr}</ErrorMsg>}
            <div className="help" id="r-z-h">Shielded testnet address from Zingo.</div>
          </div>
        </Section>
      </div>

      <div className="cw-col">
        <Section i={1} m={3}>
          <dl className="dl">
            <div className="dl-row"><dt>You redeem</dt><dd>{amtOk ? `${formatZat(n!)} cWZEC` : '—'}</dd></div>
            <div className="dl-row"><dt>Fee</dt><dd>{feeZat != null ? `${formatZat(feeZat)} ZEC` : '—'}</dd></div>
            <div className="dl-row big"><dt>You receive</dt><dd>{amtOk && feeZat != null && n! > feeZat ? `${formatZat(n! - feeZat)} ZEC` : '—'}</dd></div>
            <div className="dl-row stack"><dt>To</dt><dd>{zOk ? r.z : '—'}</dd></div>
            <div className="dl-row"><dt>Network</dt><dd>Zcash testnet</dd></div>
          </dl>
          <div className="help">Sent after Sepolia finality, usually about 15 minutes.</div>
        </Section>

        <Section i={2} m={4}>{action}</Section>

        <Section i={3} m={5}>
          <div className="fld">
            <div className="lbl-row">
              <label className="lbl" htmlFor="r-id">Redeem ID</label>
              {tracked && !r.fromFlow && <button type="button" className="btn btn-t" onClick={r.stopTracking}>Change</button>}
            </div>
            {tracked ? (
              <output id="r-id" className="ro-box">{r.trackedId}</output>
            ) : (
              <div className="hud">
                <input
                  id="r-id" className={`inp mono ${r.idErr ? 'err' : ''}`} type="text" inputMode="numeric" autoComplete="off" spellCheck={false}
                  aria-invalid={!!r.idErr} aria-describedby="r-id-m"
                  value={r.idInput} onChange={(e) => r.editId(e.target.value)}
                  onKeyDown={(e) => e.key === 'Enter' && r.validateId() && r.startTrackId()}
                />
              </div>
            )}
            {r.idErr && <ErrorMsg id="r-id-m">{r.idErr}</ErrorMsg>}
            {!tracked && !r.idErr && <div className="help" id="r-id-m">Track an existing redeem.</div>}
          </div>
          {tracked ? (
            <RedeemStatus r={r} deposit={deposit} />
          ) : (
            <div><button type="button" className="btn btn-s" onClick={() => r.validateId() && r.startTrackId()}>Track</button></div>
          )}
        </Section>
      </div>
    </div>
  )
}

function RedeemStatus({ r, deposit }: { r: Redeem; deposit: Deposit }) {
  const rec = r.status.data
  const again = r.fromFlow
    ? <button type="button" className="btn btn-s" onClick={r.reset}>New redeem</button>
    : <button type="button" className="btn btn-s" onClick={r.trackAnother}>Track another</button>
  return (
    <>
      <PollNotice poll={r.status} />
      {rec === null && <Panel kind="info" title="Not found yet" detail="It can take a few minutes to appear after the burn." />}
      {rec && <RedeemRecordView rec={rec} deposit={deposit} />}
      {rec && redeemStage(rec).kind !== 'progress' && <div>{again}</div>}
    </>
  )
}

function RedeemRecordView({ rec, deposit }: { rec: RedeemRecord; deposit: Deposit }) {
  const st = redeemStage(rec)
  const pay = rec.payment?.txid && (
    <div className="dl-row">
      <dt>Zcash transaction</dt>
      <dd><span className="inline"><span>{shortHex(rec.payment.txid)}</span><CopyTextButton text={rec.payment.txid} label="Copy" className="btn-s btn-sm" ariaLabel="Copy Zcash transaction ID" /></span></dd>
    </div>
  )
  const chainRow = (label: string, t: RedeemRecord['fulfillment']) => t && (
    <div className="dl-row"><dt>{label}</dt><dd><ExplorerLink href={explorerTx(t.transaction)} hash={shortHex(t.transaction)} what={label} /></dd></div>
  )

  if (st.kind === 'progress') {
    const other = deposit.status.data?.find((d) => depositStage(d).kind === 'progress')
    const otherStage = other && depositStage(other)
    return (
      <>
        <Progress label="Redeem progress" steps={REDEEM_STEPS} current={st.step} detail={st.detail} />
        {pay && <dl className="dl">{pay}</dl>}
        {other && otherStage?.kind === 'progress' && (
          <div className="oa" style={{ borderTop: '1px solid var(--border)', paddingTop: 24 }}>
            <span className="lbl">Other activity</span>
            <div className="oa-row">
              <span>Deposit</span><span className="mono">{shortHex(other.txid)}</span><span className="muted">· {DEPOSIT_STEPS[otherStage.step]} ·</span>
              <a href="#deposit">View</a>
            </div>
          </div>
        )}
      </>
    )
  }
  if (st.kind === 'panel') {
    return (
      <>
        <Panel kind={st.panel} title={st.title} detail={st.detail} />
        {(pay || rec.recredit) && <dl className="dl">{pay}{chainRow('Return transaction', rec.recredit)}</dl>}
      </>
    )
  }
  // paid: ZEC payment settled; fulfilled: also settled on Sepolia.
  return (
    <>
      <div className="rc-h"><Icon name="check" big /><h2 className="lg" style={{ margin: 0 }}>Paid</h2></div>
      <dl className="dl">
        {pay}
        {rec.status === 'fulfilled' && rec.fulfillment
          ? chainRow('Fulfillment', rec.fulfillment)
          : <div className="dl-row"><dt>Fulfillment</dt><dd>Pending</dd></div>}
      </dl>
    </>
  )
}
