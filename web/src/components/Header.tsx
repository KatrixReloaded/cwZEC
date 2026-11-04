import type { BridgeInfo } from '../api'
import { DEMO, explorerAddress } from '../config'
import type { Route } from '../hooks/useHashRoute'
import type { Wallet } from '../hooks/useWallet'
import { copyText, shortHex } from '../lib/format'
import { INSTALL_URL } from '../wallet/metamask'
import { Icon, Spinner } from './Icon'

export type Menu = 'testnet' | 'picker' | 'conn' | null

type Props = { route: Route; wallet: Wallet; info?: BridgeInfo; menu: Menu; setMenu: (m: Menu) => void }

export function Header({ route, wallet, info, menu, setMenu }: Props) {
  const toggle = (m: Menu) => setMenu(menu === m ? null : m)
  const nav = (
    <>
      <a href="#deposit" aria-current={route === 'deposit' ? 'page' : undefined} onClick={() => setMenu(null)}>Deposit</a>
      <a href="#swap" aria-current={route === 'swap' ? 'page' : undefined} onClick={() => setMenu(null)}>Swap</a>
      <a href="#redeem" aria-current={route === 'redeem' ? 'page' : undefined} onClick={() => setMenu(null)}>Redeem</a>
    </>
  )

  return (
    <header className="cw-hd">
      <div className="cw-hd-in">
        <span className="cw-mark">MAZE</span>
        <nav className="cw-nav" aria-label="Main">{nav}</nav>
        <span className="cw-sp" />
        <div className="cw-hd-r">
          <button type="button" className="cw-pill" aria-haspopup="dialog" aria-expanded={menu === 'testnet'} onClick={() => toggle('testnet')}>
            <Icon name="info" /><span>{DEMO ? 'Demo' : 'Testnet'}</span>
          </button>
          <WalletButton wallet={wallet} menu={menu} toggle={toggle} />
        </div>

        {menu === 'testnet' && (
          <div className="pop" role="dialog" aria-label="Testnet bridge">
            <div className="pop-h">
              <span className="w6 md">Testnet bridge</span>
              <button type="button" className="ibtn" aria-label="Close" onClick={() => setMenu(null)}><Icon name="close" /></button>
            </div>
            <span className="w5">Zcash testnet → Sepolia</span>
            <span className="muted">Run by a trusted operator who holds the vault key and can see amounts and addresses.</span>
            <span className="muted">Testnet tokens only. No real value.</span>
            {DEMO && <span className="w5">Demo mode. All results are simulated.</span>}
            {info && (
              <a className="muted mono" href={explorerAddress(info.contract)} target="_blank" rel="noreferrer">Contract {shortHex(info.contract)}</a>
            )}
          </div>
        )}

        {menu === 'picker' && wallet.status === 'none' && (
          <div className="menu" role="menu" aria-label="Choose a wallet">
            {wallet.installed ? (
              <button type="button" className="menu-item" role="menuitem" onClick={() => { setMenu(null); wallet.connect() }}>
                <span className="wico"><Icon name="wallet" /></span><span>MetaMask</span>
              </button>
            ) : (
              <a className="menu-item" role="menuitem" href={INSTALL_URL} target="_blank" rel="noreferrer" style={{ textDecoration: 'none' }} onClick={() => setMenu(null)}>
                <span className="wico"><Icon name="external" /></span><span>Install MetaMask</span>
              </a>
            )}
          </div>
        )}

        {menu === 'conn' && wallet.connected && (
          <div className="menu" role="menu" aria-label="Wallet">
            <div className="menu-addr">{wallet.address}</div>
            <div className="menu-sep" />
            <button type="button" className="menu-item" role="menuitem" onClick={() => { copyText(wallet.address!); setMenu(null) }}>Copy address</button>
            <button type="button" className="menu-item" role="menuitem" onClick={() => { setMenu(null); wallet.disconnect(true) }}>Forget on this device</button>
            <button type="button" className="menu-item" role="menuitem" onClick={() => { setMenu(null); wallet.disconnect() }}>Disconnect</button>
          </div>
        )}
      </div>
      <nav className="cw-tabs" aria-label="Main">{nav}</nav>
    </header>
  )
}

function WalletButton({ wallet, menu, toggle }: { wallet: Wallet; menu: Menu; toggle: (m: Menu) => void }) {
  switch (wallet.status) {
    case 'none':
      return (
        <button type="button" className="btn btn-s btn-hd" aria-haspopup="menu" aria-expanded={menu === 'picker'} onClick={() => toggle('picker')}>
          <Icon name="wallet" /><span className="wl-d">Connect wallet</span><span className="wl-m">Connect</span>
        </button>
      )
    case 'connecting':
      return <button type="button" className="btn btn-s btn-hd busy" aria-disabled="true"><Spinner /><span>Connecting…</span></button>
    case 'connected': {
      const short = shortHex(wallet.address!)
      return (
        <button type="button" className="btn btn-s btn-hd" aria-haspopup="menu" aria-expanded={menu === 'conn'} aria-label={`Wallet ${short}`} onClick={() => toggle('conn')}>
          <span className="mono">{short}</span><Icon name="chevron" />
        </button>
      )
    }
    case 'wrong':
      return <button type="button" className="btn btn-s btn-hd" onClick={wallet.switchNetwork}><Icon name="alert" /><span>Wrong network</span></button>
  }
}
