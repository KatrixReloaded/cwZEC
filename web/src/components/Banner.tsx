import type { InfoBanner, PanelKind } from '../lib/status'
import { Panel } from './ui'

export type BannerKey = InfoBanner | 'wrongnet'

const BANNERS: Record<BannerKey, [PanelKind, string, string?]> = {
  nym: ['alert', "Couldn't connect through Nym."],
  unavail: ['alert', 'Bridge unavailable', 'Retrying…'],
  paused: ['alert', 'Bridge is paused.'],
  notready: ['info', "Bridge isn't ready.", 'Status may be delayed.'],
  wrongnet: ['alert', 'Wrong network'],
}

export function Banner({ kind, onSwitchNetwork }: { kind: BannerKey; onSwitchNetwork: () => void }) {
  const [k, title, detail] = BANNERS[kind]
  return (
    <div className="banner-wrap">
      <Panel kind={k} title={title} detail={detail}>
        {kind === 'nym' && <button type="button" className="btn btn-p btn-sm" onClick={() => location.reload()}>Reload</button>}
        {kind === 'wrongnet' && <button type="button" className="btn btn-p btn-sm" onClick={onSwitchNetwork}>Switch to Sepolia</button>}
      </Panel>
    </div>
  )
}
