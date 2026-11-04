import { beforeEach, expect, it, vi } from 'vitest'
const transport = vi.hoisted(() => ({ fetch: vi.fn() }))
vi.mock('../api/nym', () => ({ nym: transport }))
import { quoteURL } from './quote'
beforeEach(() => transport.fetch.mockReset())
it('requires an explicit public HTTPS origin, with no credentials or path', () => {
  for (const origin of ['', 'http://example.com', 'https://localhost', 'https://127.0.0.1', 'https://example.com/path', 'https://key@example.com', 'https://example.com?key=secret']) expect(() => quoteURL(origin)).toThrow()
  expect(quoteURL('https://prices.example.com')).toBe('https://prices.example.com/quote')
})
it('does not start any transport when HTTPS is unconfigured', async () => {
  vi.stubEnv('VITE_SWAP_QUOTE_URL', '')
  vi.resetModules()
  const { fetchQuote } = await import('./quote')
  await expect(fetchQuote('0x11')).rejects.toThrow()
  vi.unstubAllEnvs()
  expect(transport.fetch).not.toHaveBeenCalled()
})
it('serializes quote requests through the shared Nym transport, without a direct fallback', async () => {
  vi.stubEnv('VITE_SWAP_QUOTE_URL', 'https://prices.example.com')
  vi.resetModules()
  const { fetchQuote: configured } = await import('./quote')
  let finish!: (r: Response) => void
  transport.fetch.mockImplementationOnce(() => new Promise<Response>(resolve => { finish = resolve }))
  transport.fetch.mockResolvedValueOnce(new Response('{}', { status: 503 }))
  const first = configured('0x11').catch(e => e)
  const second = configured('0x11').catch(e => e)
  expect(transport.fetch).toHaveBeenCalledTimes(1)
  expect(transport.fetch.mock.calls[0].slice(0, 2)).toEqual(['https://prices.example.com/quote', { headers: { accept: 'application/json' } }])
  finish(new Response('{}', { status: 503 }))
  await first; await second
  expect(transport.fetch).toHaveBeenCalledTimes(2)
  vi.unstubAllEnvs()
})
it('ignores a late body after the requesting account scope aborts', async () => {
  vi.stubEnv('VITE_SWAP_QUOTE_URL', 'https://prices.example.com')
  vi.resetModules()
  const { fetchQuote: configured } = await import('./quote')
  const abort = new AbortController()
  transport.fetch.mockImplementationOnce(async () => { abort.abort(); return new Response('{}') })
  await expect(configured('0x11', abort.signal)).rejects.toThrow('abandoned')
  vi.unstubAllEnvs()
})
