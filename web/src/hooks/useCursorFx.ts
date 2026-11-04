import { useEffect, useRef, type MouseEvent } from 'react'

type Box = { x: number; y: number; w: number; h: number; field?: boolean }

/**
 * HUD cursor: two hairlines cross at the eased pointer; a bracket reticle snaps to the nearest
 * background grid dot, or frames the hovered button / link / field. Writes transforms straight to
 * the DOM in a rAF loop — never re-renders.
 */
export function useCursorFx() {
  const root = useRef<HTMLDivElement>(null)
  const dots = useRef<HTMLDivElement>(null)
  const spot = useRef<HTMLDivElement>(null)
  const vline = useRef<HTMLDivElement>(null)
  const hline = useRef<HTMLDivElement>(null)
  const s = useRef({
    on: false, raf: 0, last: 0, tile: 20,
    tx: 0, ty: 0, cx: 0, cy: 0,
    r: null as Box | null, lock: null as Box | null,
  }).current

  useEffect(() => () => { cancelAnimationFrame(s.raf); s.raf = 0 }, [s])

  function lockBox(t: EventTarget | null): Box | null {
    const el = (t as Element | null)?.closest?.('button, a, input, textarea, [role=menuitem]')
    if (!el || !root.current?.contains(el) || el.closest('.cw-hd')) return null
    const field = el.tagName === 'INPUT' || el.tagName === 'TEXTAREA'
    const r = el.getBoundingClientRect(), R = root.current.getBoundingClientRect(), pad = field ? 10 : 6
    return { x: r.left - R.left + r.width / 2, y: r.top - R.top + r.height / 2, w: r.width + pad * 2, h: r.height + pad * 2, field }
  }

  function target(): Box {
    if (s.lock) return s.lock
    const T = s.tile, o = T / 2 - 10
    return { x: Math.round((s.tx - o) / T) * T + o, y: Math.round((s.ty - o) / T) * T + o, w: 16, h: 16 }
  }

  function paint() {
    const { cx: x, cy: y } = s
    if (vline.current) vline.current.style.transform = `translate3d(${(x - 0.5).toFixed(2)}px,${(y - 140).toFixed(2)}px,0)`
    if (hline.current) hline.current.style.transform = `translate3d(${(x - 140).toFixed(2)}px,${(y - 0.5).toFixed(2)}px,0)`
    const el = spot.current
    if (!el) return
    const r = (s.r ??= target())
    el.style.width = r.w.toFixed(2) + 'px'
    el.style.height = r.h.toFixed(2) + 'px'
    el.style.transform = `translate3d(${(r.x - r.w / 2).toFixed(2)}px,${(r.y - r.h / 2).toFixed(2)}px,0)`
  }

  /** Time-based exponential easing: identical glide at any refresh rate, no snap at the end. */
  function loop() {
    if (s.raf) return
    s.last = 0
    const step = (t: number) => {
      s.raf = 0
      const dt = s.last ? Math.min(64, t - s.last) : 16
      s.last = t
      const k = 1 - Math.exp(-dt / 90), kr = 1 - Math.exp(-dt / 60), ks = 1 - Math.exp(-dt / 85)
      s.cx += (s.tx - s.cx) * k
      s.cy += (s.ty - s.cy) * k
      const st = target()
      const r = (s.r ??= { ...st })
      r.x += (st.x - r.x) * kr
      r.y += (st.y - r.y) * kr
      r.w += (st.w - r.w) * ks
      r.h += (st.h - r.h) * ks
      paint()
      const moving =
        Math.abs(s.tx - s.cx) + Math.abs(s.ty - s.cy) > 0.05 ||
        Math.abs(st.x - r.x) + Math.abs(st.y - r.y) > 0.05 ||
        Math.abs(st.w - r.w) + Math.abs(st.h - r.h) > 0.05
      if (moving) s.raf = requestAnimationFrame(step)
    }
    s.raf = requestAnimationFrame(step)
  }

  const handlers = {
    onMouseMove(e: MouseEvent) {
      const el = root.current
      if (!el) return
      const R = el.getBoundingClientRect()
      s.tx = e.clientX - R.left
      s.ty = e.clientY - R.top
      if (!s.on) {
        // re-entering: start under the cursor instead of gliding across the page
        s.on = true
        s.cx = s.tx
        s.cy = s.ty
        s.r = null
        if (dots.current) s.tile = parseFloat(getComputedStyle(dots.current).backgroundSize) || 20
        paint()
        el.style.setProperty('--spot', '1')
      }
      s.lock = lockBox(e.target)
      spot.current?.classList.toggle('lock', !!s.lock)
      spot.current?.classList.toggle('field', !!s.lock?.field)
      vline.current?.classList.toggle('dim', !!s.lock)
      hline.current?.classList.toggle('dim', !!s.lock)
      loop()
    },
    onMouseLeave() {
      s.on = false
      root.current?.style.setProperty('--spot', '0')
    },
    /** Buttons/links send a ring outward; text fields clamp the brackets in. */
    onMouseDown(e: MouseEvent) {
      const el = spot.current, t = e.target as Element
      if (!el || t.closest?.('.cw-hd')) return
      el.classList.remove('ping', 'clamp')
      void el.offsetWidth
      el.classList.add(t.closest?.('input, textarea') ? 'clamp' : 'ping', 'press')
    },
    onMouseUp() {
      spot.current?.classList.remove('press')
    },
  }

  return { refs: { root, dots, spot, vline, hline }, handlers }
}
