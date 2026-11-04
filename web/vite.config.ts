/// <reference types="vitest/config" />
import { defineConfig } from 'vite'
import react from '@vitejs/plugin-react'

// No /v1 proxy: live API requests go to VITE_API_BASE through Nym only.
export default defineConfig({
  plugins: [react()],
  // The FHE SDK resolves its WASM and worker files relative to its own modules.
  optimizeDeps: { exclude: ['@zama-fhe/sdk', '@fhevm/sdk'] },
  test: { environment: 'node' },
})
