import { useEffect, useRef, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import { StatusCard } from './components/StatusCard'
import { Titlebar } from './components/Titlebar'
import { HARNESS_VERSION } from './generated-version'
import { subscribeTheme } from './theme'
import type { BackendStatus } from './types'
import './styles.css'

const win = getCurrentWindow()

// Fallback Harness version used when the backend hasn't reported one yet
// (e.g. during early startup or after a hard failure).  The real version
// always comes from the Rust backend via BackendStatus.  Imported from the
// auto-generated `generated-version.ts` so it stays in sync with the bundle.
const FALLBACK_HARNESS_VERSION = HARNESS_VERSION

/**
 * Opens or closes the version panel. The panel is a child WebView owned by the
 * Rust side, so its open/closed state is authoritative there; the returned
 * value and the `desktop-info` event both feed the same button state.
 */
async function toggleInfo(): Promise<boolean> {
  try {
    return await invoke<boolean>('toggle_desktop_info')
  } catch (error) {
    console.error('toggle version panel failed', error)
    return false
  }
}

const RESTARTING_STATUS: BackendStatus = {
  phase: 'starting',
  message: '正在重新启动内置 Harness…',
  harnessVersion: FALLBACK_HARNESS_VERSION,
}

function failedStatus(error: unknown): BackendStatus {
  return { phase: 'failed', message: String(error), harnessVersion: FALLBACK_HARNESS_VERSION }
}

function App() {
  const [status, setStatus] = useState<BackendStatus>({
    phase: 'starting',
    message: '正在启动内置 Node.js 与 DeepSeek Harness…',
    harnessVersion: FALLBACK_HARNESS_VERSION,
  })
  const [infoOpen, setInfoOpen] = useState(false)
  const [maximized, setMaximized] = useState(false)
  // Guards against a double click on the retry button; not render state.
  const retrying = useRef(false)

  // Single source of truth for the theme: `./theme` owns the initial read, the
  // live `harness-theme` events and the OS listener, and applies `data-theme`
  // to this document. The chrome (including the Lucide titlebar icons, which
  // follow `currentColor`) restyles through CSS custom properties, so no React
  // state is needed here.
  useEffect(() => {
    let unsubscribe: (() => void) | undefined
    let disposed = false
    void subscribeTheme(() => {}).then((detach) => {
      if (disposed) detach()
      else unsubscribe = detach
    })
    return () => {
      disposed = true
      unsubscribe?.()
    }
  }, [])

  useEffect(() => {
    const subscriptions = [
      listen<BackendStatus>('backend-status', (event) => {
        setStatus(event.payload)
      }),
      listen<boolean>('desktop-info', (event) => {
        setInfoOpen(event.payload)
      }),
    ]
    return () => {
      subscriptions.forEach((subscription) => void subscription.then((unlisten) => unlisten()))
    }
  }, [])

  // The maximize button mirrors the real window state; resize covers maximize,
  // restore, snap layouts and edge-snapping.
  useEffect(() => {
    let disposed = false
    const sync = async () => {
      const next = await win.isMaximized()
      if (!disposed) setMaximized(next)
    }
    void sync()
    const subscription = win.onResized(sync)
    return () => {
      disposed = true
      void subscription.then((unlisten) => unlisten())
    }
  }, [])

  useEffect(() => {
    invoke<BackendStatus>('backend_status')
      .then(setStatus)
      .catch((error: unknown) => {
        setStatus(failedStatus(error))
      })
  }, [])

  const handleRetry = () => {
    if (retrying.current) return
    retrying.current = true
    setStatus(RESTARTING_STATUS)
    invoke<BackendStatus>('restart_backend')
      .then(setStatus)
      .catch((error: unknown) => {
        setStatus(failedStatus(error))
      })
      .finally(() => {
        retrying.current = false
      })
  }

  return (
    <>
      <Titlebar
        infoOpen={infoOpen}
        maximized={maximized}
        onToggleInfo={() => void toggleInfo().then(setInfoOpen)}
      />
      <StatusCard status={status} onRetry={handleRetry} />
    </>
  )
}

const root = document.querySelector<HTMLElement>('#app')

if (!root) {
  throw new Error('Missing application root')
}

createRoot(root).render(<App />)
