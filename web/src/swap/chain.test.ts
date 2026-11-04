import { beforeEach, describe, expect, it, vi } from 'vitest'
import { decodeFunctionData } from 'viem'
const rpc = vi.hoisted(() => ({ readContract: vi.fn(), getChainId: vi.fn(), getCode: vi.fn(), getTransaction: vi.fn(), waitForTransactionReceipt: vi.fn(), getBlock: vi.fn(), getBlockNumber: vi.fn() }))
vi.mock('viem', async original => ({ ...await original<typeof import('viem')>(), createPublicClient: () => rpc }))
vi.mock('../wallet/metamask', () => ({ metamask: { provider: () => p } }))
import { abi, BALANCER, POOL, SIGNER, UNDERLYING, USDC, ZEC } from './core'
import { authorizationData, authorized, configuration, confirmed, loadTracking, saveTracking, send, session, trackingKey } from './chain'
const account = '0x1111111111111111111111111111111111111111'
const hash = ('0x' + 'aa'.repeat(32)) as `0x${string}`
const p = { request: vi.fn(), on: vi.fn(), removeListener: vi.fn() }
let memory: Map<string, string>
beforeEach(() => {
  vi.resetAllMocks()
  memory = new Map()
  vi.stubGlobal('localStorage', { getItem: (k: string) => memory.get(k) ?? null, setItem: (k: string, v: string) => memory.set(k, v), removeItem: (k: string) => memory.delete(k) })
  p.request.mockImplementation(async ({ method }) => ({ eth_accounts: [account], eth_chainId: '0xaa36a7', eth_getTransactionCount: '0x1', eth_sendTransaction: hash })[method as string])
})
describe('authorization and deployed configuration', () => {
  it('uses ERC7984 setOperator with an explicit 10 minute expiry', async () => {
    expect(decodeFunctionData({ abi, data: authorizationData(1000n) })).toEqual({ functionName: 'setOperator', args: [expect.stringMatching(new RegExp(POOL, 'i')), 1600] })
    rpc.readContract.mockResolvedValue(true)
    expect(await authorized(account)).toBe(true)
    expect(rpc.readContract).toHaveBeenCalledWith({ address: ZEC, abi, functionName: 'isOperator', args: [account, POOL] })
  })
  it('checks deployed contracts, fee, token decimals and wrapper backing', async () => {
    rpc.getChainId.mockResolvedValue(11155111); rpc.getCode.mockResolvedValue('0x1234')
    rpc.readContract.mockImplementation(({ address, functionName }) => ({ cwzec: ZEC, cusdc: USDC, balancer: BALANCER, quoteSigner: SIGNER, underlying: UNDERLYING, token: ZEC, decimals: address === ZEC ? 8 : 6, feeBps: 100 })[functionName as string])
    expect(await configuration()).toEqual({ fee: 100n, signer: SIGNER })
    rpc.getCode.mockResolvedValue('0x')
    await expect(configuration()).rejects.toThrow('configuration')
  })
})
describe('submission guard and recovery (simulated wallet)', () => {
  it('sends once with zero ETH and persists only public identifiers', async () => {
    await send(account, 'swap', '0x1234', () => {}, () => {})
    await expect(send(account, 'swap', '0x1234', () => {}, () => {})).rejects.toThrow('tracked')
    expect(p.request.mock.calls.filter(([a]) => a.method === 'eth_sendTransaction')).toHaveLength(1)
    expect(p.request).toHaveBeenCalledWith(expect.objectContaining({ method: 'eth_sendTransaction', params: [expect.objectContaining({ value: '0x0', to: POOL })] }))
    expect(JSON.parse(memory.get(trackingKey(account))!)).toEqual({ kind: 'swap', nonce: '1', hash })
    expect(loadTracking(USDC)).toBeNull()
  })
  it('blocks concurrent sends, including while reading nonce', async () => {
    const result = await Promise.allSettled([send(account, 'swap', '0x', () => {}, () => {}), send(account, 'swap', '0x', () => {}, () => {})])
    expect(result.filter(r => r.status === 'fulfilled')).toHaveLength(1)
  })
  it('retains an ambiguous nonce and refuses resubmission after reload', async () => {
    p.request.mockImplementation(async ({ method }) => { if (method === 'eth_sendTransaction') throw Error('disconnected'); return ({ eth_accounts: [account], eth_chainId: '0xaa36a7', eth_getTransactionCount: '0x1' })[method as string] })
    await expect(send(account, 'swap', '0x', () => {}, () => {})).rejects.toThrow('uncertain')
    expect(loadTracking(account)).toEqual({ kind: 'swap', nonce: '1' })
    await expect(send(account, 'swap', '0x', () => {}, () => {})).rejects.toThrow('tracked')
  })
  it('allows retry only after explicit wallet cancellation', async () => {
    const base = p.request.getMockImplementation()!
    p.request.mockImplementation(async args => { if (args.method === 'eth_sendTransaction') throw { code: 4001 }; return base(args) })
    await expect(send(account, 'authorization', authorizationData(1000n), () => {}, () => {})).rejects.toThrow('cancelled')
    expect(loadTracking(account)).toBeNull()
  })
  it('drops a stale account or network response before sending', async () => {
    p.request.mockResolvedValueOnce([USDC])
    await expect(session(account, p)).rejects.toThrow('Wallet changed')
    let stale = false
    const base = p.request.getMockImplementation()!
    p.request.mockImplementation(async args => { const value = await base(args); if (args.method === 'eth_getTransactionCount') stale = true; return value })
    await expect(send(account, 'swap', '0x', () => { if (stale) throw Error('stale scope') }, () => {})).rejects.toThrow('stale scope')
    expect(loadTracking(account)).toBeNull()
  })
})
describe('canonical receipts', () => {
  it('requires two confirmations, rejects a reorg and does not infer fill', async () => {
    rpc.getTransaction.mockResolvedValue({ from: account, to: POOL, nonce: 1, value: 0n, input: '0x' })
    rpc.waitForTransactionReceipt.mockResolvedValue({ blockNumber: 10n, blockHash: hash, status: 'success', logs: [] })
    rpc.getBlock.mockResolvedValue({ hash }); rpc.getBlockNumber.mockResolvedValue(11n)
    const t = { kind: 'swap' as const, nonce: '1', hash }
    const r = await confirmed(account, t, () => {})
    expect(r.receipt.status).toBe('success')
    expect(r).not.toHaveProperty('filled')
    expect(rpc.waitForTransactionReceipt).toHaveBeenCalledWith(expect.objectContaining({ confirmations: 2 }))
    rpc.getBlock.mockResolvedValue({ hash: '0x00' })
    await expect(confirmed(account, t, () => {})).rejects.toThrow('Confirmation changed')
  })
  it('recognizes a cancellation consuming the same sender nonce', async () => {
    rpc.getTransaction.mockResolvedValue({ from: account, to: account, nonce: 1, value: 0n, input: '0x' })
    rpc.waitForTransactionReceipt.mockResolvedValue({ blockNumber: 10n, blockHash: hash, status: 'success', logs: [] })
    rpc.getBlock.mockResolvedValue({ hash }); rpc.getBlockNumber.mockResolvedValue(11n)
    expect((await confirmed(account, { kind: 'swap', nonce: '1', hash }, () => {})).replaced).toBe(true)
  })
  it('rejects a recovery hash from another sender', async () => {
    saveTracking(account, { kind: 'swap', nonce: '1' })
    rpc.getTransaction.mockResolvedValue({ from: USDC, nonce: 1 })
    await expect(confirmed(account, { kind: 'swap', nonce: '1', hash }, () => {})).rejects.toThrow('does not match')
  })
})
