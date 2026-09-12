/**
 * Version / history panel.
 *
 * Renders the data returned by the `get_desktop_info` command. The panel is a
 * separate local WebView (`info`) so it stays visible above the Harness page,
 * which is why it imports the shared stylesheet instead of reusing the
 * bootstrap document.
 */
import { useEffect, useState } from 'react'
import { createRoot } from 'react-dom/client'
import { invoke } from '@tauri-apps/api/core'
import { X } from 'lucide-react'
import { InfoBody } from './components/InfoBody'
import { UpdateCheck } from './components/UpdateCheck'
import type { DesktopInfo } from './lib/desktop-info'
import { subscribeTheme } from './theme'
import './styles.css'

async function close(): Promise<void> {
  try {
    await invoke<boolean>('toggle_desktop_info')
  } catch {
    // The panel is closing anyway; a failure here only means the WebView is
    // already gone.
  }
}

function InfoPanel() {
  const [info, setInfo] = useState<DesktopInfo | null>(null)
  const [error, setError] = useState<string | null>(null)

  // The manual update check re-reads the info so the "上次更新检查"
  // timestamp stays in sync with the registry check it just performed.
  const load = () => {
    invoke<DesktopInfo>('get_desktop_info')
      .then((next) => {
        setInfo(next)
        setError(null)
      })
      .catch((cause: unknown) => {
        setError(`无法读取版本信息：${String(cause)}`)
      })
  }

  useEffect(() => {
    load()
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === 'Escape') {
        void close()
      }
    }
    document.addEventListener('keydown', onKeyDown)
    return () => document.removeEventListener('keydown', onKeyDown)
  }, [])

  // The panel is its own document, so it must resolve the Harness theme itself;
  // otherwise it keeps using the light design tokens while the rest of the shell
  // switches to dark.
  useEffect(() => {
    let unsubscribe: (() => void) | undefined
    let disposed = false
    void subscribeTheme(() => {
      // Applying `data-theme` restyles the panel through CSS custom properties,
      // so no re-render is needed here.
    }).then((detach) => {
      if (disposed) detach()
      else unsubscribe = detach
    })
    return () => {
      disposed = true
      unsubscribe?.()
    }
  }, [])

  return (
    <>
      <header className="info-header">
        <span className="info-header__title">版本信息</span>
        <button
          className="info-header__close"
          type="button"
          aria-label="关闭"
          onClick={() => void close()}
        >
          <X size={11} aria-hidden="true" />
        </button>
      </header>
      <div className="info-body">
        {error ? (
          <p className="info-error">{error}</p>
        ) : info ? (
          <>
            <InfoBody info={info} />
            <UpdateCheck onChecked={load} />
          </>
        ) : (
          <p className="info-empty">正在读取版本信息…</p>
        )}
      </div>
    </>
  )
}

const root = document.querySelector<HTMLElement>('#info-root')

if (!root) {
  throw new Error('Missing info root')
}

root.classList.add('info-panel')
createRoot(root).render(<InfoPanel />)
