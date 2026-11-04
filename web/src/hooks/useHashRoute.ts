import { useEffect, useState } from 'react'

export type Route = 'deposit' | 'redeem' | 'swap' | 'notfound'

function read(): Route {
  const h = window.location.hash.slice(1)
  if (h === '' || h === 'deposit') return 'deposit'
  return h === 'swap' ? 'swap' : h === 'redeem' ? 'redeem' : 'notfound'
}

export function useHashRoute() {
  const [route, setRoute] = useState(read)
  useEffect(() => {
    const on = () => setRoute(read())
    window.addEventListener('hashchange', on)
    return () => window.removeEventListener('hashchange', on)
  }, [])
  return route
}
