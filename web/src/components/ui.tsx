import type { CSSProperties, ReactNode } from 'react'
import { useCopied } from '../hooks/useCopied'
import type { PanelKind } from '../lib/status'
import { Icon, Spinner } from './Icon'

/** A view section. `i` / `m` are its stagger index on desktop / mobile (30ms in, 15ms out per step). */
export function Section({ i, m = i, children }: { i: number; m?: number; children: ReactNode }) {
  const style = { '--ed': `${i * 30}ms`, '--em': `${m * 30}ms`, '--xd': `${i * 15}ms`, '--xm': `${m * 15}ms` } as CSSProperties
  return <section className="cw-sec" style={style}>{children}</section>
}

export function ErrorMsg({ id, children }: { id?: string; children: ReactNode }) {
  return <div className="err-msg" id={id}><Icon name="alert" /><span>{children}</span></div>
}

export type { PanelKind }

/** Status panel / banner. Children are rendered as its action buttons. */
export function Panel({ kind, title, detail, children }: { kind: PanelKind; title: string; detail?: string; children?: ReactNode }) {
  return (
    <div className="panel" role="status">
      <Icon name={kind} big />
      <span className="t">{title}</span>
      {detail && <span className="d">{detail}</span>}
      {children && <div className="acts">{children}</div>}
    </div>
  )
}

export function Progress({ label, steps, current, detail }: { label: string; steps: string[]; current: number; detail?: string }) {
  return (
    <ol className="prog" aria-label={label}>
      {steps.map((s, i) => {
        const st = i < current ? 'done' : i === current ? 'cur' : 'up'
        return (
          <li key={s} className={`step ${st}`} aria-current={st === 'cur' ? 'step' : undefined}>
            <span className="ic">
              {st === 'done' && <Icon name="check" label="Done" />}
              {st === 'cur' && <Spinner label="In progress" />}
              {st === 'up' && <Icon name="upcoming" label="Upcoming" />}
            </span>
            <span className="t">{s}</span>
            {st === 'cur' && detail && <span className="d">{detail}</span>}
          </li>
        )
      })}
    </ol>
  )
}

export function CopyButton({ copied, onClick, describedBy, className = '' }: { copied: boolean; onClick: () => void; describedBy?: string; className?: string }) {
  return (
    <button type="button" className={`btn btn-s btn-sm ${className}`} aria-describedby={describedBy} onClick={onClick}>
      <Icon name={copied ? 'check' : 'copy'} />
      <span>{copied ? 'Copied' : 'Copy'}</span>
    </button>
  )
}

export function ExplorerLink({ href, hash, what }: { href: string; hash: string; what: string }) {
  return (
    <a href={href} target="_blank" rel="noreferrer" aria-label={`${what} ${hash} on Sepolia explorer`}>
      <span>{hash}</span>
      <Icon name="external" />
    </a>
  )
}

/** Copy button whose "Copied" state lives as long as the button does. */
export function CopyTextButton({ text, label, className = 'btn-s', ariaLabel }: { text: string; label: string; className?: string; ariaLabel?: string }) {
  const [copied, copy] = useCopied()
  return <button type="button" className={`btn ${className}`} aria-label={ariaLabel} onClick={() => copy(text)}>{copied ? 'Copied' : label}</button>
}

/** Loading, first-load failure and stale notices for a polled status. */
export function PollNotice({ poll }: { poll: { loading: boolean; stale: boolean; error?: unknown } }) {
  if (poll.loading) return <div className="val empty inline"><Spinner /><span>Checking…</span></div>
  if (poll.stale) return <div className="help">Couldn't refresh. Showing last known status.</div>
  if (poll.error) return <Panel kind="alert" title="Couldn't check status" detail="Retrying…" />
  return null
}

export const listItemStyle = (i: number): CSSProperties => ({
  display: 'flex', flexDirection: 'column', gap: 16,
  ...(i ? { borderTop: '1px solid var(--border)', paddingTop: 24 } : {}),
})
