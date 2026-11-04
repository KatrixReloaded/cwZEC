import { afterEach, expect, it, vi } from 'vitest'
import { getJson, postJson } from './http'
import { nym } from './nym'

const config = vi.hoisted(() => ({ API_BASE: '' }))
vi.mock('../config', () => config)
afterEach(() => {
  vi.restoreAllMocks()
  vi.unstubAllGlobals()
  vi.useRealTimers()
})

it('times out a stalled GET through its signal and permits a later retry', async () => {
  vi.useFakeTimers()
  config.API_BASE = 'https://api.example.test'
  vi.stubGlobal('fetch', vi.fn(() => { throw new Error('direct fetch forbidden') }))
  let requestSignal!: AbortSignal
  const send = vi.spyOn(nym, 'fetch').mockImplementationOnce((_url, _init, signal) => {
    requestSignal = signal!
    return new Promise((_resolve, reject) => signal!.addEventListener('abort', () => reject(signal!.reason), { once: true }))
  }).mockResolvedValue(new Response('{"deposits":[]}'))
  const pending = getJson('/v1/deposits/test').catch((error) => error)
  await vi.advanceTimersByTimeAsync(59_999)
  expect(requestSignal.aborted).toBe(false)
  await vi.advanceTimersByTimeAsync(1)
  expect(await pending).toMatchObject({ name: 'TimeoutError' })
  expect(requestSignal.aborted).toBe(true)
  expect(await getJson('/v1/deposits/test')).toEqual({ deposits: [] })
  expect(send).toHaveBeenCalledTimes(2)
  expect(fetch).not.toHaveBeenCalled()
  expect(vi.getTimerCount()).toBe(0)
})

it('preserves caller cancellation and clears the GET timeout', async () => {
  vi.useFakeTimers()
  config.API_BASE = 'https://api.example.test'
  vi.spyOn(nym, 'fetch').mockImplementation((_url, _init, signal) => new Promise((_resolve, reject) => {
    signal!.addEventListener('abort', () => reject(signal!.reason), { once: true })
  }))
  const controller = new AbortController()
  const pending = getJson('/v1/info', controller.signal).catch((error) => error)
  controller.abort()
  expect(await pending).toBe(controller.signal.reason)
  expect(vi.getTimerCount()).toBe(0)
})

it.each([
  ['https://caiman-wanted-fox.ngrok-free.app', '1'],
  ['https://api.example.test', undefined],
  ['https://fake-ngrok-free.app', undefined],
])('uses the ngrok API header only for ngrok hosts: %s', async (origin, expected) => {
  config.API_BASE = origin
  vi.stubGlobal('fetch', vi.fn(() => { throw new Error('direct fetch forbidden') }))
  const send = vi.spyOn(nym, 'fetch').mockImplementation(async () => new Response('{}'))
  await getJson('/v1/info')
  await postJson('/v1/redeem/prepare', { operation_id: 'test' })
  for (const [, init] of send.mock.calls) {
    expect(init.headers['ngrok-skip-browser-warning']).toBe(expected)
    expect(init.headers.accept).toBe('application/json')
  }
  expect(send.mock.calls[1][1].headers['content-type']).toBe('application/json')
  expect(fetch).not.toHaveBeenCalled()
})
