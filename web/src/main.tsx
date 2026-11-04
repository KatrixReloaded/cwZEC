import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { App, type Scheme } from './App'
import './styles/cw.css'

// ?scheme=light | dark — defaults to the cyber scheme
const scheme = (new URLSearchParams(location.search).get('scheme') ?? 'cyber') as Scheme

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App scheme={scheme} />
  </StrictMode>,
)
