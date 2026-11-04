import { useEffect, useState } from 'react'
import { DEFAULT_CHAIN_ID } from '../config'
import { metamask, type WalletSession } from '../wallet/metamask'

export type WalletStatus = 'none' | 'connecting' | 'connected' | 'wrong'

// Remembers that the user connected, so we silently restore the session on reload
// but stay disconnected after they click Disconnect.
const KEY = 'maze.wallet'
const remembered = () => {
  try {
    return !!localStorage.getItem(KEY)
  } catch {
    return false
  }
}
const remember = (on: boolean) => {
  try {
    if (on) localStorage.setItem(KEY, 'metamask')
    else localStorage.removeItem(KEY)
  } catch {}
}

/** `chainId` is the decimal chain ID from /v1/info. */
export function useWallet(chainId = DEFAULT_CHAIN_ID) {
  const [session, setSession] = useState<WalletSession | null>(null)
  const [connecting, setConnecting] = useState(false)
  const expected = '0x' + BigInt(chainId).toString(16)
  const status: WalletStatus = connecting ? 'connecting' : !session ? 'none' : session.chainId.toLowerCase() === expected ? 'connected' : 'wrong'

  useEffect(() => {
    let live = true
    const sync = async () => {
      if (!remembered()) return
      const s = await metamask.session()
      if (!s) remember(false)
      if (live) setSession(s)
    }
    sync()
    const off = metamask.subscribe(sync)
    return () => {
      live = false
      off()
    }
  }, [])

  async function connect() {
    setConnecting(true)
    try {
      const s = await metamask.connect()
      remember(!!s)
      setSession(s)
    } catch {
      // cancelled or failed: stay disconnected
    } finally {
      setConnecting(false)
    }
  }

  async function switchNetwork() {
    try {
      await metamask.switchChain(expected)
    } catch {
      return
    }
    setSession(await metamask.session())
  }

  async function disconnect(forget = false) {
    remember(false)
    if (forget) await metamask.forget()
    setSession(null)
  }

  return { status, address: session?.address ?? null, connected: status === 'connected', installed: metamask.installed(), connect, switchNetwork, disconnect }
}

export type Wallet = ReturnType<typeof useWallet>
