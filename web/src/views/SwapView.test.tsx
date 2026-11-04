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
  state = { amount: '', slippage: '50', config: null, permission: false, message: '', readError: '', busy: false, tracking: null } as ReturnType<typeof useSwap>
  vi.mocked(useSwap).mockImplementation(() => state)
})

it('lets authorization retry when background configuration has not loaded', () => {
  state.readError = 'Wallet RPC unavailable'
  const html = render()
  expect(html).toMatch(/<button class="btn btn-s">Authorize operator/)
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

it('keeps authorization disabled while a transaction needs recovery', () => {
  state.tracking = { kind: 'authorization', nonce: '4' }
  const html = render()
  expect(html).toMatch(/<button class="btn btn-s" disabled="">Authorize operator/)
  expect(html).toContain('Outcome uncertain')
})
