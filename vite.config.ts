import { resolve } from 'node:path'
import react from '@vitejs/plugin-react'
import { defineConfig } from 'vite'

export default defineConfig({
  clearScreen: false,
  plugins: [react()],
  server: {
    port: 1420,
    strictPort: true,
    watch: {
      ignored: ['**/src-tauri/**'],
    },
  },
  envPrefix: ['VITE_', 'TAURI_'],
  build: {
    target: process.env.TAURI_ENV_PLATFORM === 'windows' ? 'chrome105' : 'safari13',
    minify: process.env.TAURI_ENV_DEBUG ? false : 'esbuild',
    sourcemap: Boolean(process.env.TAURI_ENV_DEBUG),
    rollupOptions: {
      // Two local documents ship with the app: the bootstrap shell that owns
      // the title bar, and the version panel that renders above the Harness
      // WebView. They are separate child WebViews, so both need real entries.
      input: {
        main: resolve(import.meta.dirname, 'index.html'),
        info: resolve(import.meta.dirname, 'info.html'),
      },
    },
  },
})
