import { nym } from '../api/nym'
import { validateQuote } from './core'
/** Public origin only. The Nym exit cannot reach loopback; never fall back to native fetch. */
export const QUOTE_BASE = (import.meta.env.VITE_SWAP_QUOTE_URL ?? '').replace(/\/$/, '')
export function quoteURL(base = QUOTE_BASE) {
  const u = new URL(base)
  if (u.protocol !== 'https:' || u.username || u.password || u.port || u.pathname !== '/' || u.search || u.hash || /^(localhost|127\.|\[::1\])/.test(u.hostname)) throw Error('Configure a public HTTPS quote origin')
  return u.origin + '/quote'
}
async function requestQuote(signer: string, signal?: AbortSignal) {
  const url = quoteURL()
  const response = await nym.fetch(url, { headers: { accept: 'application/json' } }, signal ? AbortSignal.any([signal, AbortSignal.timeout(60000)]) : AbortSignal.timeout(60000))
  if (!response.ok) throw Error('Price unavailable')
  const body = await response.text()
  if (body.length > 16384 || signal?.aborted) throw Error('Invalid or abandoned quote')
  return validateQuote(JSON.parse(body), signer)
}

let pending: Promise<Awaited<ReturnType<typeof requestQuote>>> | undefined
export async function fetchQuote(signer: string, signal?: AbortSignal): ReturnType<typeof requestQuote> {
  while (pending) { try { await pending } catch { /* previous request finished */ } }
  if (signal?.aborted) throw signal.reason
  const request = requestQuote(signer, signal)
  pending = request
  try { return await request } finally { if (pending === request) pending = undefined }
}
