import { useEffect, useState, type ReactNode } from 'react'
import { Backdrop } from './components/Backdrop'
import { Banner } from './components/Banner'
import { Header, type Menu } from './components/Header'
import { useBalance } from './hooks/useBalance'
import { useBridgeInfo } from './hooks/useBridgeInfo'
import { useCursorFx } from './hooks/useCursorFx'
import { useDeposit } from './hooks/useDeposit'
import { useHashRoute } from './hooks/useHashRoute'
import { useRedeem } from './hooks/useRedeem'
import { useViewTransition, type ViewPhase } from './hooks/useViewTransition'
import { useWallet } from './hooks/useWallet'
import { DepositView } from './views/DepositView'
import { NotFound } from './views/NotFound'
import { RedeemView } from './views/RedeemView'

export type Scheme = 'cyber' | 'dark' | 'light'

const SR_ONLY = { position: 'absolute', width: 1, height: 1, overflow: 'hidden', clip: 'rect(0 0 0 0)', whiteSpace: 'nowrap', margin: 0 } as const

function ViewFrame({ title, phase, children }: { title: string; phase: ViewPhase; children: ReactNode }) {
  if (phase === 'gone') return null
  return (
    <div className={`cw-view ${phase}`} aria-hidden={phase !== 'in'}>
      <h1 style={SR_ONLY}>{title}</h1>
      {children}
    </div>
  )
}

export function App({ scheme = 'cyber' }: { scheme?: Scheme }) {
  const route = useHashRoute()
  const { phases, veil } = useViewTransition(route)
  const cursor = useCursorFx()
  const bridge = useBridgeInfo()
  const chainId = bridge.info?.chain_id
  const wallet = useWallet(chainId && /^\d+$/.test(chainId) ? chainId : undefined)
  const info = bridge.info
  const balance = useBalance(wallet.connected && wallet.address && info ? { account: wallet.address, chainId: info.chain_id, contract: info.contract } : null)
  const deposit = useDeposit(wallet)
  const redeem = useRedeem(balance, bridge, wallet)
  const [menu, setMenu] = useState<Menu>(null)
  const [booting, setBooting] = useState(true)

  useEffect(() => {
    const t = setTimeout(() => setBooting(false), 1400)
    return () => clearTimeout(t)
  }, [])
  useEffect(() => setMenu(null), [route])

  const banner = wallet.status === 'wrong' ? 'wrongnet' : bridge.banner
  const openPicker = () => setMenu('picker')
  const cls = ['cw', scheme === 'dark' ? '' : scheme, booting && 'cw-boot'].filter(Boolean).join(' ')

  return (
    <div className={cls} ref={cursor.refs.root} {...cursor.handlers}>
      <Backdrop refs={cursor.refs} veil={veil} />
      <Header route={route} wallet={wallet} info={bridge.info} menu={menu} setMenu={setMenu} />
      {banner && <Banner kind={banner} onSwitchNetwork={wallet.switchNetwork} />}
      <main className="cw-main">
        {route === 'notfound' ? (
          <NotFound />
        ) : (
          <div className="cw-views">
            <ViewFrame title="Deposit" phase={phases.deposit}>
              <DepositView d={deposit} wallet={wallet} balance={balance} bridge={bridge} onConnect={openPicker} />
            </ViewFrame>
            <ViewFrame title="Redeem" phase={phases.redeem}>
              <RedeemView r={redeem} wallet={wallet} balance={balance} deposit={deposit} bridge={bridge} onConnect={openPicker} />
            </ViewFrame>
          </div>
        )}
      </main>
    </div>
  )
}
