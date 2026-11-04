import { formatZat, parseZec } from './amount'

/** `min` comes from /v1/info; `balance` only once revealed. Empty input is not an error. */
export function amountError(v: string, min: bigint | null, balance: bigint | null) {
  v = v.trim()
  if (!v) return ''
  if (!/^\d*\.?\d*$/.test(v) || v === '.') return 'Enter an amount.'
  if ((v.split('.')[1] || '').length > 8) return 'Use up to 8 decimal places.'
  const n = parseZec(v)!
  if (n === 0n) return 'Enter an amount greater than 0.'
  if (min != null && n < min) return `Minimum is ${formatZat(min)} cWZEC.`
  if (balance != null && n > balance) return 'More than your balance.'
  return ''
}

export function zaddrError(v: string) {
  v = v.trim()
  if (!v) return ''
  if (/^t[m123]/.test(v)) return "Use a shielded address. Transparent addresses aren't supported."
  if (/^z/.test(v)) return 'Use a unified address starting with utest1.'
  if (/^u1/.test(v)) return 'This is a mainnet address. Use a testnet address.'
  if (/^utest1[02-9ac-hj-np-z]{20,}$/.test(v)) return ''
  return 'Check the Zcash address.'
}

export const isRedeemId = (v: string) => /^(0|[1-9]\d{0,77})$/.test(v)
