import { afterEach, describe, expect, it, vi } from 'vitest'
import { fulfilled, info, minted } from '../test/fixtures'
import { HttpError } from './http'
import { liveApi } from './live'
import { nym } from './nym'

const API = 'https://api.example.test'
vi.mock('../config', async (orig) => ({ ...(await orig<object>()), API_BASE: 'https://api.example.test' }))

const reply = (status: number, body: unknown) => new Response(JSON.stringify(body), { status, headers: { 'content-type': 'application/json' } })
/** Replaces the Nym transport; native fetch must never be used. */
const mockFetch = (...responses: (Response | Error)[]) => {
  vi.stubGlobal('fetch', vi.fn(() => Promise.reject(new Error('native fetch used'))))
  const fn = vi.spyOn(nym, 'fetch')
  for (const r of responses) r instanceof Error ? fn.mockRejectedValueOnce(r) : fn.mockResolvedValueOnce(r)
  return fn
}
afterEach(() => {
  expect(fetch).not.toHaveBeenCalled()
  vi.unstubAllGlobals()
  vi.restoreAllMocks()
})

describe('liveApi', () => {
  it('sends a plain public GET without credentials or API key', async () => {
    const f = mockFetch(reply(200, info))
    expect(await liveApi.getInfo()).toEqual(info)
    const [url, init] = f.mock.calls[0]
    expect(url).toBe(`${API}/v1/info`)
    expect(init).toEqual({ headers: { accept: 'application/json' } })
    expect(JSON.stringify(init.headers)).not.toMatch(/api-key/i)
  })

  it('passes the txid through unchanged and returns every deposit', async () => {
    const second = { ...minted, action_index: 1, deposit_id: '0x01', status: 'minting' }
    const f = mockFetch(reply(200, { deposits: [minted, second] }))
    const out = await liveApi.getDeposits(minted.txid)
    expect(f.mock.calls[0][0]).toBe(`${API}/v1/deposits/${minted.txid}`)
    expect(out.map((d) => [d.action_index, d.status])).toEqual([[0, 'minted'], [1, 'minting']])
  })

  it('treats an empty or missing deposits array as not detected', async () => {
    mockFetch(reply(200, { deposits: [] }), reply(200, {}))
    expect(await liveApi.getDeposits(minted.txid)).toEqual([])
    expect(await liveApi.getDeposits(minted.txid)).toEqual([])
  })

  it('maps redeem 404 to null but keeps other failures as errors', async () => {
    mockFetch(reply(200, fulfilled), reply(404, { error: 'not_found' }), reply(503, { error: 'service_unavailable' }), new TypeError('Failed to fetch'))
    expect(await liveApi.getRedeem('1')).toEqual(fulfilled)
    expect(await liveApi.getRedeem('999')).toBeNull()
    await expect(liveApi.getRedeem('1')).rejects.toEqual(new HttpError(503, 'service_unavailable'))
    await expect(liveApi.getRedeem('1')).rejects.toThrow('Failed to fetch')
  })

  it('reports non-JSON error bodies with the HTTP status', async () => {
    mockFetch(new Response('bad gateway', { status: 502 }))
    await expect(liveApi.getInfo()).rejects.toMatchObject({ status: 502, code: 'http_error' })
  })

  it('requests the memo by GET with an encoded recipient and binds it to that recipient', async () => {
    const r = '0x1111111111111111111111111111111111111111'
    const f = mockFetch(reply(200, { memo: `cwzec:v1:11155111:${r}:b34b7399` }), reply(200, { memo: 'cwzec:v1:11155111:0x2222222222222222222222222222222222222222:00000000' }), reply(400, { error: 'invalid_request' }))
    expect(await liveApi.getDepositMemo(r)).toBe(`cwzec:v1:11155111:${r}:b34b7399`)
    expect(f.mock.calls[0][0]).toBe(`${API}/v1/memo?recipient=${r}`)
    expect(f.mock.calls[0][1].method).toBeUndefined()
    await expect(liveApi.getDepositMemo(r)).rejects.toMatchObject({ code: 'invalid_response' })
    await expect(liveApi.getDepositMemo(r)).rejects.toMatchObject({ status: 400 })
  })

  it('posts the prepare body as JSON with string amounts', async () => {
    const body = { operation_id: 'web-1', user: '0x12ab34cd56ef7890a1b2c3d4e5f60718293a4b5c', amount: '150000000', zcash_address: 'utest1x', chain_id: '11155111', contract: '0x91cac58cea711bd9487721824ea38e2b5742253f' }
    const f = mockFetch(reply(200, { operation_id: 'web-1', already_submitted: true, transaction_hash: '0x' + '00'.repeat(32) }), reply(409, { error: 'operation_blocked_or_terms_conflict' }))
    expect(await liveApi.prepareRedeem(body)).toMatchObject({ already_submitted: true })
    const [url, init] = f.mock.calls[0]
    expect(url).toBe(`${API}/v1/redeem/prepare`)
    expect(init).toMatchObject({ method: 'POST', headers: { accept: 'application/json', 'content-type': 'application/json' } })
    expect(JSON.parse(init.body!)).toEqual(body)
    await expect(liveApi.prepareRedeem(body)).rejects.toMatchObject({ status: 409 })
  })

  it('treats an unfollowed redirect as an error, and passes the abort signal to the transport', async () => {
    const f = mockFetch(new Response(null, { status: 302 }))
    const controller = new AbortController()
    await expect(liveApi.getInfo(controller.signal)).rejects.toEqual(new HttpError(302, 'http_error'))
    const forwarded = f.mock.calls[0][2]!
    expect(forwarded.aborted).toBe(false)
    controller.abort()
    expect(forwarded.aborted).toBe(true)
    expect(forwarded.reason).toBe(controller.signal.reason)
  })

  it('surfaces tunnel errors as they are', async () => {
    mockFetch(new Error('too many redirects (>0)'))
    await expect(liveApi.getInfo()).rejects.toThrow('too many redirects')
  })
})
