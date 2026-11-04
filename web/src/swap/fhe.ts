import { MemoryStorage, ZamaSDK, isEncryptedValueZero, sepolia as fheSepolia } from '@zama-fhe/sdk'
import { createConfig } from '@zama-fhe/sdk/viem'
import { web } from '@zama-fhe/sdk/web'
import { createPublicClient, createWalletClient, custom, getAddress, type Address, type EIP1193Provider, type Hex } from 'viem'
import { sepolia } from 'viem/chains'
import { metamask } from '../wallet/metamask'
import { session } from './chain'
import { MAX, POOL, USDC } from './core'
async function withSDK<T>(seller: string, run: (sdk: ZamaSDK, check: () => void) => Promise<T>, signal?: AbortSignal): Promise<T> {
  let cancelled = false
  const wallet = metamask.provider()
  const guard = () => { if (cancelled || signal?.aborted) throw Error('Wallet scope changed') }
  guard(); await session(seller); guard()
  const provider = { on: wallet.on.bind(wallet), removeListener: wallet.removeListener.bind(wallet), request: async (args: Parameters<typeof wallet.request>[0]) => {
    guard()
    if (args.method.includes('sign')) { await session(seller); guard() }
    return wallet.request(args)
  } } as unknown as EIP1193Provider
  const transport = custom(provider)
  const sdk = new ZamaSDK(createConfig({
    chains: [{ ...fheSepolia, network: provider }],
    publicClient: createPublicClient({ chain: sepolia, transport }),
    walletClient: createWalletClient({ account: getAddress(seller), chain: sepolia, transport }),
    ethereum: provider, relayers: { [fheSepolia.id]: web({ offloadEncrypt: false }) },
    storage: new MemoryStorage(), transportKeyPairTTL: 86400, permitTTL: 1, telemetry: false,
  }))
  let terminated = false
  const terminate = () => { cancelled = true; if (!terminated) { terminated = true; sdk.terminate() } }
  const off = metamask.subscribe(terminate)
  signal?.addEventListener('abort', terminate, { once: true })
  try { guard(); const value = await run(sdk, guard); guard(); return value }
  finally { off(); signal?.removeEventListener('abort', terminate); terminate() }
}
export const encrypt = (seller: string, amount: bigint, minimum: bigint, signal?: AbortSignal) => withSDK(seller, sdk => sdk.encrypt({
  values: [{ type: 'euint64', value: amount }, { type: 'euint64', value: minimum }], contractAddress: POOL, userAddress: getAddress(seller),
}), signal)
export const decryptPayment = (seller: string, handle: Hex, signal?: AbortSignal) => withSDK(seller, async (sdk, check) => {
  await sdk.permits.grantPermit([USDC]); check()
  const value = (await sdk.decryption.decryptValues([{ encryptedValue: handle, contractAddress: USDC }]))[handle]
  if (typeof value !== 'bigint' || value < 0n || value > MAX) throw Error('Payment could not be verified')
  return value
}, signal)

export const revealBalance = (seller: string, contract: Address, phase: (p: 'signing' | 'decrypting') => void, signal?: AbortSignal) => withSDK(seller, async (sdk, check) => {
  const handle = await sdk.createToken(contract).confidentialBalanceOf(getAddress(seller))
  check()
  if (isEncryptedValueZero(handle)) return 0n
  phase('signing')
  await sdk.permits.grantPermit([contract]); check()
  phase('decrypting')
  const value = (await sdk.decryption.decryptValues([{ encryptedValue: handle, contractAddress: contract }]))[handle]
  if (typeof value !== 'bigint' || value < 0n || value > MAX) throw Error('Balance reveal unavailable')
  return value
}, signal)
