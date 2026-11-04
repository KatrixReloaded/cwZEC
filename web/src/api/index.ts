import { DEMO } from '../config'
import { liveApi } from './live'
import { mockApi } from './mock'
import type { BridgeApi } from './types'

export * from './types'
export { HttpError } from './http'

export const api: BridgeApi = DEMO ? mockApi : liveApi
