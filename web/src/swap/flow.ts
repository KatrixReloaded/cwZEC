import { encodeFunctionData } from 'viem'
import { abi, fresh, terms } from './core'
import { authorized, configuration, send, session, type Tracking } from './chain'
import { fetchQuote } from './quote'
/** The accepted minimum is captured at click time and never recalculated from a later quote. */
export async function executeSwap(account: string, accepted: ReturnType<typeof terms>, fee: bigint, check: () => void, status: (s: string) => void, track: (t: Tracking) => void, signal?: AbortSignal) {
  await session(account); check()
  const c = await configuration(); check()
  if (c.fee !== fee) throw Error('Fee changed — review updated terms')
  if (!await authorized(account)) throw Error('Operator authorization required')
  check(); status('Refreshing signed price through Nym…')
  const q = await fetchQuote(c.signer, signal); check(); fresh(q.quote)
  status('Encrypting amount and accepted minimum…')
  const { encrypt } = await import('./fhe'); check()
  const encrypted = await encrypt(account, accepted.amount, accepted.minimum, signal); check()
  await session(account); check(); fresh(q.quote)
  if (!await authorized(account)) throw Error('Operator authorization expired')
  check(); fresh(q.quote)
  const data = encodeFunctionData({ abi, functionName: 'swap', args: [encrypted.encryptedValues[0], encrypted.encryptedValues[1], encrypted.inputProof, q.quote.expiresAt, q.quote, q.signature] })
  status('Confirm Swap in MetaMask; quote expires within 60 seconds')
  return send(account, 'swap', data, () => { check(); fresh(q.quote) }, track)
}
