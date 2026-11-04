/** The wallet's account or network no longer matches the prepared transaction. Nothing was sent. */
export class WalletMismatchError extends Error {}

/** EIP-1193 errors that guarantee nothing was sent: user rejection, unauthorized, request already pending. */
const NOT_SENT = new Set([4001, 4100, -32002])
const code = (e: unknown) => (e as { code?: unknown })?.code

export const walletNotSent = (e: unknown) => e instanceof WalletMismatchError || NOT_SENT.has(code(e) as number)
export const walletRejected = (e: unknown) => code(e) === 4001 || (e as Error)?.name === 'SigningRejectedError'
