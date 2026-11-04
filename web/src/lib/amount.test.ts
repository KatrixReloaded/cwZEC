import { describe, expect, it } from 'vitest'
import { formatZat, parseZec, toZat } from './amount'
import { amountError } from './validate'

describe('formatZat', () => {
  it('formats exact eight-decimal amounts without floats', () => {
    expect(formatZat('10000')).toBe('0.0001')
    expect(formatZat('10001')).toBe('0.00010001')
    expect(formatZat(150_000_000n)).toBe('1.5')
    expect(formatZat(0n)).toBe('0.0')
    expect(formatZat('2100000000000000')).toBe('21000000.0')
    expect(formatZat('123456789012345678901')).toBe('1234567890123.45678901')
  })
})

describe('parseZec', () => {
  it('parses decimals into zatoshis', () => {
    expect(parseZec('1.5')).toBe(150_000_000n)
    expect(parseZec('.00000001')).toBe(1n)
    expect(parseZec('0.1')).toBe(10_000_000n) // no 0.1 float drift
  })
  it('rejects malformed input and excess precision', () => {
    for (const v of ['', '.', 'abc', '1e5', '-1', '0.000000001']) expect(parseZec(v)).toBeNull()
  })
})

describe('toZat', () => {
  it('accepts only integer strings', () => {
    expect(toZat('10001')).toBe(10001n)
    for (const v of [undefined, null, '', '1.5', '-1', 10001]) expect(toZat(v)).toBeNull()
  })
})

describe('amountError', () => {
  it('uses the minimum and balance in zatoshis', () => {
    expect(amountError('0.0001', 10001n, null)).toBe('Minimum is 0.00010001 cWZEC.')
    expect(amountError('0.00010001', 10001n, null)).toBe('')
    expect(amountError('2', 10001n, 150_000_000n)).toBe('More than your balance.')
    expect(amountError('0', 10001n, null)).toBe('Enter an amount greater than 0.')
    expect(amountError('1.123456789', 10001n, null)).toBe('Use up to 8 decimal places.')
    expect(amountError('1', null, null)).toBe('')
  })
})
