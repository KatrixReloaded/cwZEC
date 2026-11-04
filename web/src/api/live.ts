import { HttpError, getJson, postJson } from './http'
import type { BridgeApi, BridgeInfo, DepositRecord, PrepareResponse, RedeemRecord } from './types'

export const liveApi: BridgeApi = {
  getInfo: (signal) => getJson<BridgeInfo>('/v1/info', signal),
  async getDeposits(txid, signal) {
    const r = await getJson<{ deposits?: DepositRecord[] }>(`/v1/deposits/${encodeURIComponent(txid)}`, signal)
    return Array.isArray(r.deposits) ? r.deposits : []
  },
  async getRedeem(id, signal) {
    try {
      return await getJson<RedeemRecord>(`/v1/redeems/${encodeURIComponent(id)}`, signal)
    } catch (e) {
      if (e instanceof HttpError && e.status === 404) return null
      throw e
    }
  },
  async getDepositMemo(recipient, signal) {
    const r = await getJson<{ memo?: unknown }>(`/v1/memo?recipient=${encodeURIComponent(recipient)}`, signal)
    // cwzec:v1:{chain}:{lowercase recipient}:{checksum} — reject a memo for anyone else.
    const parts = typeof r.memo === 'string' ? r.memo.split(':') : []
    if (parts.length !== 5 || parts[0] !== 'cwzec' || parts[1] !== 'v1' || parts[3] !== recipient.toLowerCase())
      throw new HttpError(200, 'invalid_response')
    return r.memo as string
  },
  prepareRedeem: (req, signal) => postJson<PrepareResponse>('/v1/redeem/prepare', req, signal),
}
