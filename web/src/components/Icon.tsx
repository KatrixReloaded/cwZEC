import type { SVGProps } from 'react'

const PATHS = {
  info: <><circle cx="8" cy="8" r="6.25" /><path d="M8 7.5v3.75" /><path d="M8 4.75h.01" /></>,
  alert: <><circle cx="8" cy="8" r="6.25" /><path d="M8 4.75v3.75" /><path d="M8 11.25h.01" /></>,
  check: <path d="M3 8.5l3.25 3.25L13 5" />,
  copy: <><rect x="5.5" y="5.5" width="8.5" height="8.5" rx="1.5" /><path d="M10.5 5.5V3.5A1.5 1.5 0 0 0 9 2H3.5A1.5 1.5 0 0 0 2 3.5V9a1.5 1.5 0 0 0 1.5 1.5h2" /></>,
  wallet: <><path d="M12.75 5.25V4a1 1 0 0 0-1-1h-8.5A1.25 1.25 0 0 0 2 4.25v7.5A1.25 1.25 0 0 0 3.25 13h9.5a1 1 0 0 0 1-1v-1.5" /><path d="M14 6.25h-3a1.75 1.75 0 0 0 0 3.5h3z" /></>,
  chevron: <path d="M4.5 6.25L8 9.75l3.5-3.5" />,
  close: <path d="M4 4l8 8M12 4l-8 8" />,
  external: <><path d="M9.5 2.5h4v4" /><path d="M13.5 2.5l-6 6" /><path d="M11.5 9.5v3a1 1 0 0 1-1 1h-7a1 1 0 0 1-1-1v-7a1 1 0 0 1 1-1h3" /></>,
  upcoming: <circle cx="8" cy="8" r="4.75" />,
}

export type IconName = keyof typeof PATHS

/** 16px stroke icon; `big` renders the 20px panel/heading variant. Decorative unless a label is given. */
export function Icon({ name, big, label, ...rest }: { name: IconName; big?: boolean; label?: string } & SVGProps<SVGSVGElement>) {
  return (
    <svg
      className={big ? 'i20' : 'i'} viewBox="0 0 16 16" fill="none" stroke="currentColor"
      strokeWidth={big ? 1.2 : 1.5} strokeLinecap="round" strokeLinejoin="round"
      {...(label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': true })}
      {...rest}
    >
      {PATHS[name]}
    </svg>
  )
}

/** Rotating arc; reduced-motion CSS swaps it for a static dot. */
export function Spinner({ label }: { label?: string }) {
  const a11y = label ? { role: 'img', 'aria-label': label } : { 'aria-hidden': true }
  return (
    <>
      <svg className="i spin" viewBox="0 0 16 16" fill="none" stroke="currentColor" strokeWidth="1.5" strokeLinecap="round" {...a11y}>
        <path d="M8 1.75a6.25 6.25 0 1 1-6.25 6.25" />
      </svg>
      <svg className="i spin-dot" viewBox="0 0 16 16" {...a11y}>
        <circle cx="8" cy="8" r="3" fill="currentColor" />
      </svg>
    </>
  )
}
