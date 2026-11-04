export const shortHex = (a: string) => a.slice(0, 6) + '…' + a.slice(-4)
export const stripSpaces = (v: string) => v.replace(/\s+/g, '')
export const isEthAddress = (a: string) => /^0x[0-9a-fA-F]{40}$/.test(a)
export const isZcashTxId = (v: string) => /^[0-9a-fA-F]{64}$/.test(v)

export function copyText(text: string) {
  navigator.clipboard?.writeText(text).catch(() => {})
}
