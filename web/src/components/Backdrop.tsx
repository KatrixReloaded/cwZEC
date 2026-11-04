import type { useCursorFx } from '../hooks/useCursorFx'

type Refs = ReturnType<typeof useCursorFx>['refs']

/** Dot grid, cursor hairlines and reticle, plus the page-transition veil when one is running. */
export function Backdrop({ refs, veil }: { refs: Refs; veil: { key: number; dir: string } | null }) {
  return (
    <>
      <div className="cw-bg" aria-hidden="true">
        <div className="cw-dots" ref={refs.dots} />
        <div className="cw-glow" ref={refs.vline} />
        <div className="cw-hline" ref={refs.hline} />
      </div>
      <div className="cw-spot" aria-hidden="true" ref={refs.spot}><i className="cw-ret" /><i className="cw-ping" /></div>
      {veil && (
        <div key={veil.key} className={`cw-tx ${veil.dir}`} aria-hidden="true">
          <div className="cw-tx-glow" /><div className="cw-tx-grid" /><div className="cw-tx-dots" />
        </div>
      )}
    </>
  )
}
