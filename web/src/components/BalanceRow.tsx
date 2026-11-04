import { formatZat } from '../lib/amount'
import type { Balance } from '../hooks/useBalance'
import { Icon, Spinner } from './Icon'
import { ErrorMsg } from './ui'

/** Balance body (without its label). Pass `connected=false` to show the connect hint. */
export function BalanceRow({ balance, connected }: { balance: Balance; connected: boolean }) {
  if (!connected) return <div className="bal-row"><span className="muted">Connect a wallet to see your balance.</span></div>
  switch (balance.state) {
    case 'encrypted':
      return (
        <div className="bal-row">
          <span className="md muted">Encrypted</span>
          {balance.canReveal
            ? <button type="button" className="btn btn-s btn-sm" onClick={balance.reveal}>Reveal</button>
            : <span className="help">Connect on Sepolia to reveal.</span>}
        </div>
      )
    case 'signing':
      return (
        <>
          <div className="bal-row">
            <button type="button" className="btn btn-s btn-sm busy" aria-disabled="true"><Icon name="wallet" /><span>Confirm in wallet</span></button>
          </div>
          <div className="help">Sign the permit in your wallet. No gas.</div>
        </>
      )
    case 'decrypting':
      return <div className="bal-row"><span className="inline muted"><Spinner /><span>Decrypting…</span></span></div>
    case 'revealed':
      return (
        <div className="bal-row">
          <span className="xl mono">{formatZat(balance.amount ?? 0n)} cWZEC</span>
          <button type="button" className="btn btn-t" onClick={balance.hide}>Hide</button>
        </div>
      )
    case 'failed':
    case 'rejected':
      return (
        <div className="bal-row">
          <ErrorMsg>{balance.state === 'rejected' ? 'Permit not signed.' : "Couldn't reveal balance."}</ErrorMsg>
          <button type="button" className="btn btn-s btn-sm" onClick={balance.reveal}>Retry</button>
        </div>
      )
  }
}
