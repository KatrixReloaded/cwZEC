/** Minimal EIP-1193 provider surface we use. */
interface Eip1193Provider {
  isMetaMask?: boolean
  request(args: { method: string; params?: unknown[] }): Promise<unknown>
  on(event: string, fn: () => void): void
  removeListener(event: string, fn: () => void): void
}

declare global {
  interface Window {
    ethereum?: Eip1193Provider
  }
}

export interface WalletSession {
  address: string
  /** hex, as reported by the wallet */
  chainId: string
}

export const INSTALL_URL = 'https://metamask.io/download/'

// EIP-6963 discovery picks MetaMask even when several wallets are installed;
// window.ethereum is the fallback for older setups.
let announced: Eip1193Provider | undefined
window.addEventListener('eip6963:announceProvider', (e) => {
  const d = (e as CustomEvent).detail
  if (d?.info?.rdns === 'io.metamask') announced = d.provider
})
window.dispatchEvent(new Event('eip6963:requestProvider'))

function provider() {
  const p = announced ?? (window.ethereum?.isMetaMask ? window.ethereum : undefined)
  if (!p) throw new Error('MetaMask is not installed')
  return p
}

async function read(p: Eip1193Provider, accountsMethod: string): Promise<WalletSession | null> {
  const [address] = (await p.request({ method: accountsMethod })) as string[]
  if (!address) return null
  const chainId = (await p.request({ method: 'eth_chainId' })) as string
  return { address, chainId }
}

export type { Eip1193Provider }

export const metamask = {
  installed: () => !!(announced ?? window.ethereum?.isMetaMask),
  provider,

  /** Prompts the user to connect. Rejects if they decline. */
  connect: () => read(provider(), 'eth_requestAccounts'),

  /** Current session without prompting; null if not connected or not installed. */
  async session() {
    try {
      return await read(provider(), 'eth_accounts')
    } catch {
      return null
    }
  },

  async switchChain(chainId: string) {
    await provider().request({ method: 'wallet_switchEthereumChain', params: [{ chainId }] })
  },

  /** Revokes this site's account permission in MetaMask. */
  async forget() {
    await provider()
      .request({ method: 'wallet_revokePermissions', params: [{ eth_accounts: {} }] })
      .catch(() => {})
  },

  /** Calls `fn` whenever the account or network changes in the wallet. */
  subscribe(fn: () => void) {
    if (!this.installed()) return () => {}
    const p = provider()
    p.on('accountsChanged', fn)
    p.on('chainChanged', fn)
    return () => {
      p.removeListener('accountsChanged', fn)
      p.removeListener('chainChanged', fn)
    }
  },
}
