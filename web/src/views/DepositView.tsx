import type { DepositRecord } from '../api'
import { explorerTx } from '../config'
import type { Balance } from '../hooks/useBalance'
import type { BridgeInfoState } from '../hooks/useBridgeInfo'
import type { Deposit } from '../hooks/useDeposit'
import { useFx } from '../hooks/useFx'
import type { Wallet } from '../hooks/useWallet'
import { copyText, shortHex } from '../lib/format'
import { DEPOSIT_STEPS, depositStage } from '../lib/status'
import { BalanceRow } from '../components/BalanceRow'
import { Icon, Spinner } from '../components/Icon'
import { CopyButton, CopyTextButton, ErrorMsg, ExplorerLink, Panel, PollNotice, Progress, Section, listItemStyle } from '../components/ui'

type Props = { d: Deposit; wallet: Wallet; balance: Balance; bridge: BridgeInfoState; onConnect: () => void }

export function DepositView({ d, wallet, balance, bridge, onConnect }: Props) {
  const fx = useFx()
  const tracking = d.trackedTx != null
  const records = d.status.data
  const allMinted = !!records?.length && records.every((r) => r.status === 'minted')
  const vault = bridge.info?.deposit_address
  const bothCopied = d.addrCopied && d.memoCopied

  return (
    <div className="cw-grid">
      <div className="cw-col">
        <Section i={0}>
          <div className="fld">
            <div className="lbl-row">
              <label className="lbl" htmlFor="d-rec">Recipient</label>
              {d.recFromWallet && <button type="button" className="btn btn-t" onClick={d.useAnother}>Use another address</button>}
              {!d.recFromWallet && wallet.connected && <button type="button" className="btn btn-t" onClick={d.useConnected}>Use connected wallet</button>}
            </div>
            {d.recFromWallet ? (
              <output id="d-rec" className="ro-box">{d.recipient}</output>
            ) : (
              <div style={{ display: 'flex', flexWrap: 'wrap', gap: 8, alignItems: 'flex-start' }}>
                <div className="hud" style={{ flex: '1 1 280px', minWidth: 0 }}>
                  <textarea
                    id="d-rec" className={`inp mono ta ${d.recErr ? 'err' : ''}`} rows={1} spellCheck={false} autoComplete="off"
                    aria-invalid={!!d.recErr} aria-describedby="d-rec-m"
                    value={d.recText} onChange={(e) => d.editRecipient(e.target.value)} onBlur={d.blurRecipient}
                  />
                </div>
                {(wallet.status === 'none' || wallet.status === 'connecting') && (
                  <button type="button" className="btn btn-s btn-go" onClick={onConnect}>
                    <Icon name="wallet" /><span>{wallet.status === 'connecting' ? 'Connecting…' : 'Connect wallet'}</span>
                  </button>
                )}
              </div>
            )}
            {!d.recFromWallet && (d.recErr
              ? <ErrorMsg id="d-rec-m">{d.recErr}</ErrorMsg>
              : <div className="help" id="d-rec-m">You'll need this wallet to see the balance.</div>)}
          </div>
        </Section>

        <Section i={1}>
          <div className="fld">
            <div className="lbl-row">
              <span className="lbl" id="d-addr-l">Send to</span>
              {vault && <CopyButton copied={d.addrCopied} describedBy="d-addr-l" onClick={() => (copyText(vault), d.markAddrCopied())} />}
            </div>
            {vault ? (
              <div className={`val ${d.addrCopied ? 'copied' : ''}`} aria-labelledby="d-addr-l">{vault}</div>
            ) : bridge.error ? (
              <div className="val empty">Bridge unavailable.</div>
            ) : (
              <div className="val empty inline"><Spinner /><span>Loading…</span></div>
            )}
          </div>
        </Section>

        <Section i={2}>
          <div className="fld">
            <div className="lbl-row">
              <span className="lbl" id="d-memo-l">Memo</span>
              {d.memo && <CopyButton copied={d.memoCopied} describedBy="d-memo-l" onClick={() => (copyText(d.memo!), d.markMemoCopied())} />}
            </div>
            {d.memoState === 'ready' && <div className={`val ${d.memoCopied ? 'copied' : ''}`} aria-labelledby="d-memo-l">{d.memo}</div>}
            {d.memoState === 'empty' && <div className="val empty">Enter a recipient first.</div>}
            {d.memoState === 'loading' && <div className="val empty inline"><Spinner /><span>Loading…</span></div>}
            {d.memoState === 'failed' && (
              <div className="bal-row">
                <ErrorMsg>Couldn't create the memo.</ErrorMsg>
                <button type="button" className="btn btn-s btn-sm" onClick={d.retryMemo}>Retry</button>
              </div>
            )}
          </div>
          {d.memo && vault && (
            <div className="instr">
              {bothCopied ? <Icon name="check" label="Address and memo copied" /> : <Icon name="info" />}
              <span>In Zingo, send shielded testnet ZEC to this address with this memo. The memo must match exactly.</span>
            </div>
          )}
        </Section>
      </div>

      <div className="cw-col">
        {!allMinted && (
          <Section i={1} m={3}>
            <div className="fld">
              <div className="lbl-row">
                <label className="lbl" htmlFor="d-tx">Zcash transaction ID</label>
                {tracking && <button type="button" className="btn btn-t" onClick={d.changeTx}>Change</button>}
              </div>
              {tracking ? (
                <output id="d-tx" className="ro-box">{d.trackedTx}</output>
              ) : (
                <div className="hud">
                  <textarea
                    id="d-tx" className={`inp mono ta ${d.txErr ? 'err' : ''}`} rows={1} spellCheck={false} autoComplete="off"
                    aria-invalid={!!d.txErr} aria-describedby="d-tx-h"
                    value={d.tx} onChange={(e) => d.editTx(e.target.value)}
                  />
                </div>
              )}
              {d.txErr && <ErrorMsg>{d.txErr}</ErrorMsg>}
              {!tracking && <div className="help" id="d-tx-h">Copy it from Zingo after sending.</div>}
            </div>
            {!tracking && (
              <div>
                <button type="button" className={`btn btn-p btn-x ${fx.cls}`} onClick={() => (d.validateTrack() ? fx.burst(d.startTrack) : fx.shake())}>Track</button>
              </div>
            )}
          </Section>
        )}

        {tracking && (
          <Section i={2} m={4}>
            <PollNotice poll={d.status} />
            {records?.length === 0 && (
              <Panel kind="info" title="Not detected yet" detail="Check the transaction ID, the address and the memo in Zingo.">
                <button type="button" className="btn btn-s" onClick={d.changeTx}>Change</button>
              </Panel>
            )}
            {records?.map((r, i) => (
              <DepositItem key={`${r.deposit_id}:${r.action_index}`} rec={r} index={i} count={records.length} />
            ))}
            {allMinted && (
              <>
                <div className="fld" style={{ borderTop: '1px solid var(--border)', paddingTop: 24 }}>
                  <span className="lbl">Balance</span>
                  <BalanceRow balance={balance} connected={wallet.connected} />
                </div>
                <div><button type="button" className="btn btn-s" onClick={d.trackAnother}>Track another</button></div>
              </>
            )}
          </Section>
        )}
      </div>
    </div>
  )
}

function DepositItem({ rec, index, count }: { rec: DepositRecord; index: number; count: number }) {
  const st = depositStage(rec)
  const mintRow = rec.mint_transaction && (
    <div className="dl-row"><dt>Mint transaction</dt><dd><ExplorerLink href={explorerTx(rec.mint_transaction)} hash={shortHex(rec.mint_transaction)} what="Mint transaction" /></dd></div>
  )
  return (
    <div style={listItemStyle(index)}>
      {count > 1 && <span className="lbl">Deposit {index + 1} of {count}</span>}
      {st.kind === 'progress' && (
        <>
          <Progress label="Deposit progress" steps={DEPOSIT_STEPS} current={st.step} detail={st.detail} />
          {mintRow && <dl className="dl">{mintRow}</dl>}
        </>
      )}
      {st.kind === 'panel' && (
        <Panel kind={st.panel} title={st.title} detail={st.detail}>
          {rec.status === 'failed' && <CopyTextButton text={rec.txid} label="Copy transaction ID" />}
        </Panel>
      )}
      {st.kind === 'done' && (
        <>
          <div className="rc-h"><Icon name="check" big /><h2 className="lg" style={{ margin: 0 }}>Minted</h2></div>
          <dl className="dl">
            {mintRow}
            <div className="dl-row">
              <dt>Zcash transaction</dt>
              <dd><span className="inline"><span>{shortHex(rec.txid)}</span><CopyTextButton text={rec.txid} label="Copy" className="btn-s btn-sm" ariaLabel="Copy Zcash transaction ID" /></span></dd>
            </div>
          </dl>
        </>
      )}
    </div>
  )
}
