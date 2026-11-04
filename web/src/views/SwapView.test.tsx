import { beforeEach, expect, it, vi } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import type { Wallet } from '../hooks/useWallet'
import { useSwap } from '../swap/useSwap'
import { SwapView } from './SwapView'

vi.mock('../swap/useSwap', () => ({ useSwap: vi.fn() }))
vi.mock('../swap/usePrivateBalance', () => ({
  usePrivateBalance: () => ({ known: null, canReveal: false, state: 'idle', hide: vi.fn() }),
}))
vi.mock('../config', () => ({ DEMO: false, explorerTx: (hash: string) => hash }))

const wallet = { connected: true, address: '0x1111111111111111111111111111111111111111' } as Wallet
let state: ReturnType<typeof useSwap>
const render = () => renderToStaticMarkup(<SwapView wallet={wallet} onConnect={vi.fn()} />)
beforeEach(() => {
  state = { amount: '', slippage: '50', config: null, permission: false, message: '', readError: '', busy: false, preparingSwap: false, tracking: null } as ReturnType<typeof useSwap>
  vi.mocked(useSwap).mockImplementation(() => state)
})

it('lets authorization retry when background configuration has not loaded', () => {
  state.readError = 'Wallet RPC unavailable'
  const html = render()
  expect(html).toMatch(/<button class="btn btn-s">Enable swapping/)
  expect(html).toContain('Wallet RPC unavailable')
  expect(html.indexOf('Wallet RPC unavailable')).toBeLessThan(html.indexOf('Private wallet balances'))
  expect(html).toMatch(/<button class="btn btn-p btn-x" disabled="">Swap/)
})

it('shows progress beside the action and prevents duplicate clicks while checking', () => {
  state.busy = true
  state.message = 'Checking wallet and Swap contracts…'
  state.readError = 'Old background error'
  const html = render()
  expect(html).toMatch(/<button class="btn btn-s" disabled="">Working…/)
  expect(html).toContain('Checking wallet and Swap contracts…')
  expect(html).not.toContain('Old background error')
})

it('offers an explicit same-nonce retry for a restored authorization', () => {
  state.tracking = { kind: 'authorization', nonce: '4' }
  const html = render()
  expect(html).toMatch(/<button class="btn btn-s">Retry enabling swaps/)
  expect(html).toContain('Reuses the previous nonce')
  expect(html).toContain('No transaction hash saved')
  expect(html).toMatch(/<button class="btn btn-p btn-x" disabled="">Swap/)
})

it('keeps swap recovery separate and never offers to retry an uncertain swap', () => {
  state.tracking = { kind: 'swap', nonce: '4' }
  const html = render()
  expect(html).toMatch(/<button class="btn btn-s" disabled="">Enable swapping/)
  expect(html).toContain('Outcome uncertain')
  expect(html).not.toContain('Retry enabling swaps')
})

it.each(['authorization', 'swap'] as const)('allows amount entry with hidden balances and a restored %s transaction', kind => {
  state.tracking = { kind, nonce: '4' }
  state.busy = true // Confirmation checks do not freeze the next draft.
  const html = render()
  expect(html.match(/<input id="swap-amount"[^>]*>/)?.[0]).not.toContain('disabled')
  expect(html.match(/<select id="swap-slippage"[^>]*>/)?.[0]).not.toContain('disabled')
  expect(html).toContain('Encrypted')
  expect(html).toMatch(/<button class="btn btn-p btn-x" disabled="">Swap/)
})

it('freezes the amount and slippage while preparing and sending a swap', () => {
  state.preparingSwap = true
  state.busy = true
  const html = render()
  expect(html.match(/<input id="swap-amount"[^>]*>/)?.[0]).toContain('disabled')
  expect(html.match(/<select id="swap-slippage"[^>]*>/)?.[0]).toContain('disabled')
})
