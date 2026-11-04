import { createPublicClient, custom, decodeFunctionData, encodeFunctionData, getAddress, type Address, type EIP1193Provider, type Hex } from 'viem'
import { sepolia } from 'viem/chains'
import { metamask, type Eip1193Provider } from '../wallet/metamask'
import { abi, BALANCER, CHAIN, POOL, SIGNER, UNDERLYING, USDC, ZEC } from './core'
export function client() { return createPublicClient({ chain: sepolia, transport: custom(metamask.provider() as unknown as EIP1193Provider, { retryCount: 0 }) }) }
export async function session(account: string, p: Eip1193Provider = metamask.provider()) {
  const accounts = await p.request({ method: 'eth_accounts' }) as string[]
  const chain = await p.request({ method: 'eth_chainId' }) as string
  if (accounts[0]?.toLowerCase() !== account.toLowerCase() || BigInt(chain) !== BigInt(CHAIN)) throw Error('Wallet changed — reconnect on Sepolia')
}
export async function configuration() {
  const c = client()
  if (await c.getChainId() !== CHAIN) throw Error('Switch to Sepolia')
  const read = (address: Address, functionName: 'cwzec' | 'cusdc' | 'balancer' | 'quoteSigner' | 'underlying' | 'token') => c.readContract({ address, abi, functionName })
  const values = await Promise.all([read(POOL, 'cwzec'), read(POOL, 'cusdc'), read(POOL, 'balancer'), read(POOL, 'quoteSigner'), read(USDC, 'underlying'), read(BALANCER, 'token')])
  if (values.some((v, i) => v.toLowerCase() !== [ZEC, USDC, BALANCER, SIGNER, UNDERLYING, ZEC][i])) throw Error('Deployed Swap configuration mismatch')
  const [z, u, fee, ...code] = await Promise.all([
    c.readContract({ address: ZEC, abi, functionName: 'decimals' }), c.readContract({ address: USDC, abi, functionName: 'decimals' }),
    c.readContract({ address: POOL, abi, functionName: 'feeBps' }),
    ...[POOL, ZEC, USDC, BALANCER, UNDERLYING].map(address => c.getCode({ address })),
  ])
  if (z !== 8 || u !== 6 || fee > 1000 || code.some(v => !v || v === '0x')) throw Error('Invalid deployed configuration')
  return { fee: BigInt(fee), signer: values[3] }
}
export const authorized = (account: string) => client().readContract({ address: ZEC, abi, functionName: 'isOperator', args: [getAddress(account), POOL] })
export const authorizationData = (time: bigint) => encodeFunctionData({ abi, functionName: 'setOperator', args: [POOL, Number(time + 600n)] })
export type Tracking = { kind: 'authorization' | 'swap'; nonce: string; hash?: Hex }
export const trackingKey = (account: string) => `maze.swap:${CHAIN}:${POOL}:${account.toLowerCase()}`
export function loadTracking(account: string): Tracking | null {
  try {
    const t = JSON.parse(localStorage.getItem(trackingKey(account)) ?? 'null')
    return t && ['authorization', 'swap'].includes(t.kind) && /^\d+$/.test(t.nonce) && (!t.hash || /^0x[\da-f]{64}$/i.test(t.hash)) ? t : null
  } catch { return null }
}
export function saveTracking(account: string, t: Tracking) { localStorage.setItem(trackingKey(account), JSON.stringify(t)) }
export function clearTracking(account: string) { localStorage.removeItem(trackingKey(account)) }
/** Persist the public nonce BEFORE entering MetaMask. An ambiguous send is never retried. */
export async function send(account: string, kind: Tracking['kind'], data: Hex, current: () => void, onTrack: (t: Tracking) => void, p = metamask.provider()) {
  current(); await session(account, p); current()
  if (loadTracking(account)) throw Error('Resolve the tracked transaction first')
  const nonce = await p.request({ method: 'eth_getTransactionCount', params: [account, 'pending'] }) as string
  current()
  if (loadTracking(account)) throw Error('Resolve the tracked transaction first')
  const tracking: Tracking = { kind, nonce: BigInt(nonce).toString() }
  saveTracking(account, tracking); onTrack(tracking)
  try {
    const hash = await p.request({ method: 'eth_sendTransaction', params: [{ from: account, to: kind === 'swap' ? POOL : ZEC, data, value: '0x0', chainId: '0xaa36a7', nonce }] })
    if (typeof hash !== 'string' || !/^0x[\da-f]{64}$/i.test(hash)) throw Error('Unknown wallet response')
    tracking.hash = hash as Hex
    saveTracking(account, tracking); onTrack(tracking)
    return tracking
  } catch (e) {
    if ((e as { code?: number }).code === 4001) { clearTracking(account); onTrack({ ...tracking, nonce: '' }); throw Error('Request cancelled in MetaMask') }
    throw Error('Transaction outcome uncertain — check MetaMask activity; do not resubmit')
  }
}
export async function confirmed(account: string, tracking: Tracking, onTrack: (t: Tracking) => void) {
  if (!tracking.hash) throw Error('Outcome uncertain — enter the transaction hash from MetaMask activity')
  const c = client()
  const tx = await c.getTransaction({ hash: tracking.hash })
  if (tx.from.toLowerCase() !== account.toLowerCase() || tx.nonce !== Number(tracking.nonce)) throw Error('Transaction does not match this tracking record')
  let replaced = tx.to?.toLowerCase() !== (tracking.kind === 'swap' ? POOL : ZEC) || tx.value !== 0n
  try { replaced ||= decodeFunctionData({ abi, data: tx.input }).functionName !== (tracking.kind === 'swap' ? 'swap' : 'setOperator') } catch { replaced = true }
  const receipt = await c.waitForTransactionReceipt({ hash: tracking.hash, confirmations: 2, timeout: 180000, onReplaced: r => {
    replaced ||= r.reason !== 'repriced' || r.transaction.input !== tx.input || r.transaction.to !== tx.to || r.transaction.value !== 0n
    tracking = { ...tracking, hash: r.transaction.hash }; saveTracking(account, tracking); onTrack(tracking)
  } })
  const block = await c.getBlock({ blockNumber: receipt.blockNumber })
  if (block.hash !== receipt.blockHash || await c.getBlockNumber({ cacheTime: 0 }) < receipt.blockNumber + 1n) throw Error('Confirmation changed — check again')
  return { receipt, replaced }
}
