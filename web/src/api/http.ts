import { API_BASE } from '../config'
import { nym, type MixInit } from './nym'

export class HttpError extends Error {
  constructor(readonly status: number, readonly code: string) {
    super(`${status} ${code}`)
  }
}

/**
 * Public routes only, always through the Nym tunnel; there is no direct-fetch fallback.
 * No credentials or API key are sent (mix-fetch doesn't share the browser cookie jar), and the
 * tunnel is set up with no redirects, so any 3xx either rejects or fails `res.ok`.
 */
async function request<T>(path: string, init: MixInit, signal?: AbortSignal): Promise<T> {
  if (!API_BASE.startsWith('https://')) throw new HttpError(0, 'api_not_configured')
  const headers: Record<string, string> = { accept: 'application/json', ...init.headers }
  // Free ngrok endpoints can return an HTML warning instead of the API response.
  if (new URL(API_BASE).hostname.endsWith('.ngrok-free.app')) headers['ngrok-skip-browser-warning'] = '1'
  const res = await nym.fetch(API_BASE + path, { ...init, headers }, signal)
  const body = await res.json().catch(() => null)
  if (!res.ok) throw new HttpError(res.status, body?.error ?? 'http_error')
  if (body == null) throw new HttpError(res.status, 'invalid_response')
  return body as T
}

export async function getJson<T>(path: string, signal?: AbortSignal): Promise<T> {
  const deadline = new AbortController()
  const timer = setTimeout(() => deadline.abort(new DOMException('Status request timed out', 'TimeoutError')), 60_000)
  try {
    return await request<T>(path, { headers: {} }, signal ? AbortSignal.any([signal, deadline.signal]) : deadline.signal)
  } finally {
    clearTimeout(timer)
  }
}

export const postJson = <T>(path: string, body: unknown, signal?: AbortSignal) =>
  request<T>(path, { method: 'POST', headers: { 'content-type': 'application/json' }, body: JSON.stringify(body) }, signal)
