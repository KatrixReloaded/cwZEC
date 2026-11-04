import type { Hex } from 'viem'
import { DEMO } from '../config'
import type { Receipt, WalletTx } from '../lib/redeemTx'
import { WalletMismatchError } from '../lib/walletErrors'
import { metamask } from './metamask'
import { mockChain } from './mockChain'

export interface RevealScope {
  account: string
  contract: string
  /** decimal */
  chainId: string
}
export type RevealPhase = 'signing' | 'decrypting'

/** Wallet-side operations: the only place transactions are sent or permits signed. */
export interface ChainClient {
  sendTransaction(tx: WalletTx): Promise<Hex>
  getReceipt(hash: string, signal?: AbortSignal): Promise<Receipt | null>
  revealBalance(scope: RevealScope, onPhase: (p: RevealPhase) => void): Promise<bigint>
}

const liveChain: ChainClient = {
  async sendTransaction(tx) {
    const p = metamask.provider()
    const [account] = (await p.request({ method: 'eth_accounts' })) as string[]
    const chainId = (await p.request({ method: 'eth_chainId' })) as string
    if (account?.toLowerCase() !== tx.from.toLowerCase() || BigInt(chainId) !== BigInt(tx.chainId)) throw new WalletMismatchError()
    const hash = await p.request({ method: 'eth_sendTransaction', params: [{ from: tx.from, to: tx.to, data: tx.data, value: tx.value }] })
    if (typeof hash !== 'string' || !/^0x[0-9a-fA-F]{64}$/.test(hash)) throw new Error('unexpected wallet response')
    return hash.toLowerCase() as Hex
  },
  async getReceipt(hash) {
    return ((await metamask.provider().request({ method: 'eth_getTransactionReceipt', params: [hash] })) as Receipt | null) ?? null
  },
  async revealBalance(scope, onPhase) {
    // The FHE SDK and its WASM load only when a balance is revealed.
    const { revealBalance } = await import('./fhe')
    return revealBalance(metamask.provider(), scope, onPhase)
  },
}

export const chain: ChainClient = DEMO ? mockChain : liveChain
