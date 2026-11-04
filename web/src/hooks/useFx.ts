import { useEffect, useRef, useState } from 'react'

/** Delay before acting on a primary click, so the burst plays before the button swaps out. */
const HOLD_MS = 340
const FX_MS = 600

/**
 * Click feedback for the `.btn-x` buttons: `burst` (success, then runs the action after a short hold)
 * or `shake` (validation error). Alternating A/B classes restart the animation on repeat clicks.
 */
export function useFx() {
  const [fx, setFx] = useState({ n: 0, err: false, on: false })
  const timers = useRef<number[]>([])
  useEffect(() => () => timers.current.forEach(clearTimeout), [])

  function fire(err: boolean) {
    setFx((f) => ({ n: f.n + 1, err, on: true }))
    timers.current.push(window.setTimeout(() => setFx((f) => ({ ...f, on: false })), FX_MS))
  }

  return {
    cls: fx.on ? (fx.err ? 's' : 'p') + (fx.n % 2 ? 'A' : 'B') : '',
    burst(then?: () => void) {
      fire(false)
      if (then) timers.current.push(window.setTimeout(then, HOLD_MS))
    },
    shake: () => fire(true),
  }
}
