const ZAT = 100_000_000n

/** Exact decimal string for a zatoshi amount: up to 8 decimals, trailing zeros trimmed, at least one decimal. */
export function formatZat(zat: bigint | string) {
  const n = BigInt(zat)
  const a = n < 0n ? -n : n
  const frac = (a % ZAT).toString().padStart(8, '0').replace(/0+$/, '')
  return `${n < 0n ? '-' : ''}${a / ZAT}.${frac || '0'}`
}

/** Parses a plain decimal ZEC amount into zatoshis; null if malformed or over 8 decimals. */
export function parseZec(v: string): bigint | null {
  const m = /^(\d*)(?:\.(\d*))?$/.exec(v.trim())
  if (!m || (!m[1] && !m[2]) || (m[2] ?? '').length > 8) return null
  return BigInt(m[1] || '0') * ZAT + BigInt((m[2] ?? '').padEnd(8, '0'))
}

/** Strict non-negative integer string → bigint; null for anything else. */
export const toZat = (s: unknown) => (typeof s === 'string' && /^\d+$/.test(s) ? BigInt(s) : null)
