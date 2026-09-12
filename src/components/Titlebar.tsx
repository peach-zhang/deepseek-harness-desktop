import { getCurrentWindow } from '@tauri-apps/api/window'
import { Copy, Info, Minus, Square, X } from 'lucide-react'

const win = getCurrentWindow()

interface TitlebarProps {
  infoOpen: boolean
  maximized: boolean
  onToggleInfo: () => void
}

// Lucide icons inherit `currentColor`, so the chrome follows `data-theme`
// through CSS custom properties with no light/dark asset pairs to swap.
export function Titlebar({ infoOpen, maximized, onToggleInfo }: TitlebarProps) {
  return (
    <div className="titlebar" data-tauri-drag-region>
      <span className="titlebar__title" data-tauri-drag-region>
        DSH Desktop
      </span>
      <div className="titlebar__controls">
        <button
          className={`titlebar__btn titlebar__btn--info${infoOpen ? ' is-active' : ''}`}
          id="win-info"
          aria-label="版本信息"
          aria-expanded={infoOpen}
          onClick={onToggleInfo}
        >
          <Info size={14} aria-hidden="true" focusable="false" />
        </button>
        <button
          className="titlebar__btn"
          id="win-minimize"
          aria-label="最小化"
          onClick={() => void win.minimize()}
        >
          <Minus size={12} aria-hidden="true" />
        </button>
        <button
          className="titlebar__btn"
          id="win-maximize"
          aria-label="最大化"
          onClick={() => void win.toggleMaximize()}
        >
          {maximized ? <Copy size={12} aria-hidden="true" /> : <Square size={12} aria-hidden="true" />}
        </button>
        <button
          className="titlebar__btn titlebar__btn--close"
          id="win-close"
          aria-label="关闭"
          // Closing hides the window; the embedded Harness keeps running and
          // the tray icon (or relaunching the app) brings it back.
          onClick={() => void win.hide()}
        >
          <X size={12} aria-hidden="true" />
        </button>
      </div>
    </div>
  )
}
