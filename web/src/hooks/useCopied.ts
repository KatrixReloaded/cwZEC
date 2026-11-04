import { useState } from 'react'
import { copyText } from '../lib/format'

/** Local "Copied" flag for one-off copy buttons. */
export function useCopied() {
  const [copied, setCopied] = useState(false)
  return [copied, (text: string) => (copyText(text), setCopied(true))] as const
}
