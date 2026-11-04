import { beforeEach, expect, it, vi } from 'vitest'
import { getAddress } from 'viem'
const sdk = vi.hoisted(() => ({ createToken: vi.fn(), encrypt: vi.fn(), permits: { grantPermit: vi.fn() }, decryption: { decryptValues: vi.fn() }, terminate: vi.fn() }))
const config = vi.hoisted(() => vi.fn((c: unknown) => c))
const wallet = vi.hoisted(() => ({ request: vi.fn(), on: vi.fn(), removeListener: vi.fn() }))
vi.mock('./chain', () => ({ session: vi.fn() }))
vi.mock('@zama-fhe/sdk', () => ({ ZamaSDK: vi.fn(function () { return sdk }), MemoryStorage: class MemoryStorage {}, isEncryptedValueZero: (h: string) => /^0x0{64}$/.test(h), sepolia: { id: 11155111 } }))
vi.mock('@zama-fhe/sdk/viem', () => ({ createConfig: config }))
vi.mock('@zama-fhe/sdk/web', () => ({ web: (o: unknown) => o }))
vi.mock('../wallet/metamask', () => ({ metamask: { provider: () => wallet, subscribe: () => () => {} } }))
import { decryptPayment, encrypt, revealBalance } from './fhe'
import { POOL, USDC } from './core'
const seller = '0x1111111111111111111111111111111111111111'
const handle = ('0x' + 'ab'.repeat(32)) as `0x${string}`
beforeEach(() => vi.clearAllMocks())
it('encrypts both uint64 inputs together bound to the pool and seller using SDK 3.7.1', async () => {
  sdk.encrypt.mockResolvedValue({ encryptedValues: [handle, handle], inputProof: '0x12' })
  await encrypt(seller, 100000000n, 49252500n)
  expect(sdk.encrypt).toHaveBeenCalledWith({ values: [{ type: 'euint64', value: 100000000n }, { type: 'euint64', value: 49252500n }], contractAddress: POOL, userAddress: getAddress(seller) })
  const c = config.mock.calls[0][0] as { storage: object; chains: { network: unknown }[]; telemetry: boolean }
  expect(c.storage.constructor.name).toBe('MemoryStorage'); expect(c.chains[0].network).toHaveProperty('request'); expect(c.telemetry).toBe(false)
  expect(sdk.terminate).toHaveBeenCalledOnce()
})
it('decrypts the specific payment, including zero, with a cUSDC permit', async () => {
  sdk.decryption.decryptValues.mockResolvedValue({ [handle]: 0n })
  expect(await decryptPayment(seller, handle)).toBe(0n)
  expect(sdk.permits.grantPermit).toHaveBeenCalledWith([USDC])
  expect(sdk.decryption.decryptValues).toHaveBeenCalledWith([{ encryptedValue: handle, contractAddress: USDC }])
  expect(sdk.terminate).toHaveBeenCalledOnce()
})
it('always destroys SDK material when the user rejects a permit', async () => {
  sdk.permits.grantPermit.mockRejectedValueOnce(Error('rejected'))
  await expect(decryptPayment(seller, handle)).rejects.toThrow('rejected')
  expect(sdk.terminate).toHaveBeenCalledOnce()
})

it('terminates immediately on scope abort and drops a late permit response', async () => {
  const abort = new AbortController()
  let finish!: () => void
  sdk.permits.grantPermit.mockImplementationOnce(() => new Promise<void>(resolve => { finish = resolve }))
  const result = decryptPayment(seller, handle, abort.signal)
  await vi.waitFor(() => expect(sdk.permits.grantPermit).toHaveBeenCalledOnce())
  abort.abort()
  expect(sdk.terminate).toHaveBeenCalledOnce()
  finish()
  await expect(result).rejects.toThrow('scope changed')
  expect(sdk.decryption.decryptValues).not.toHaveBeenCalled()
  expect(sdk.terminate).toHaveBeenCalledOnce()
})
it('reveals an untouched zero wallet balance without a permit', async () => {
  sdk.createToken.mockReturnValue({ confidentialBalanceOf: vi.fn().mockResolvedValue('0x' + '00'.repeat(32)) })
  expect(await revealBalance(seller, USDC, () => {})).toBe(0n)
  expect(sdk.permits.grantPermit).not.toHaveBeenCalled()
})
