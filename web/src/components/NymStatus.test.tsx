import { expect, it, vi } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { createTransport, nym, type NymState } from '../api/nym'
import { NymStatus, nymStatusLabel } from './NymStatus'

const base: NymState = { phase: 'ready', since: 0, pending: 0, waitingSince: 0, requestFailed: false }
it('distinguishes loading, slow startup, waiting and a failed request', () => {
  expect(nymStatusLabel({ ...base, phase: 'loading' }, 2)).toContain('Loading')
  expect(nymStatusLabel({ ...base, phase: 'connecting' }, 5)).toContain('Connecting')
  expect(nymStatusLabel({ ...base, phase: 'connecting' }, 31)).toContain('Still connecting')
  expect(nymStatusLabel({ ...base, pending: 1 }, 4)).toContain('Waiting')
  expect(nymStatusLabel({ ...base, pending: 1 }, 20)).toContain('taking longer')
  expect(nymStatusLabel({ ...base, requestFailed: true }, 0)).toContain('Request failed')
  expect(nymStatusLabel(base, 0)).toBe('Nym · Ready')
})

it('offers reload only for setup failure and does not initialize a tunnel to render status', () => {
  const setup = vi.fn()
  const transport = createTransport(setup)
  const snapshot = vi.spyOn(nym, 'getSnapshot').mockImplementation(transport.getSnapshot)
  try {
    expect(renderToStaticMarkup(<NymStatus />)).toContain('Not started')
    expect(setup).not.toHaveBeenCalled()
    snapshot.mockReturnValue({ ...base, phase: 'failed' })
    const html = renderToStaticMarkup(<NymStatus />)
    expect(html).toContain('Connection failed')
    expect(html).toContain('Reload')
    snapshot.mockReturnValue({ ...base, pending: 1, waitingSince: Date.now() })
    expect(renderToStaticMarkup(<NymStatus />)).not.toContain('Reload')
  } finally { snapshot.mockRestore() }
})
