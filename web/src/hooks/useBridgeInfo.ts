import { api } from '../api'
import { toZat } from '../lib/amount'
import { infoBanner } from '../lib/status'
import { usePoll } from './usePoll'

export function useBridgeInfo() {
  const poll = usePoll('info', (_, signal) => api.getInfo(signal))
  const info = poll.data
  return {
    ...poll,
    info,
    banner: infoBanner(info, poll.error),
    feeZat: toZat(info?.fixed_fee_zat),
    minZat: toZat(info?.minimum_redeem_zat),
    halted: !!info?.persisted?.halted,
  }
}

export type BridgeInfoState = ReturnType<typeof useBridgeInfo>
