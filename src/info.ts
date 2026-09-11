/**
 * Version / history panel.
 *
 * Renders the data returned by the `get_desktop_info` command. The panel is a
 * separate local WebView (`info`) so it stays visible above the Harness page,
 * which is why it imports the shared stylesheet instead of reusing the
 * bootstrap document.
 */
import { escapeHtml } from './bootstrap-utils'
import { renderInfoBody, type DesktopInfo } from './info-utils'
import { subscribeTheme } from './theme'
import { invoke } from '@tauri-apps/api/core'
import './styles.css'

const root = document.querySelector<HTMLElement>('#info-root')

if (!root) {
  throw new Error('Missing info root')
}
const infoRoot: HTMLElement = root
infoRoot.classList.add('info-panel')

function render(body: string): void {
  infoRoot.innerHTML = `
    <header class="info-header">
      <span class="info-header__title">版本信息</span>
      <button class="info-header__close" id="info-close" type="button" aria-label="关闭">✕</button>
    </header>
    <div class="info-body">${body}</div>
  `

  document.querySelector<HTMLButtonElement>('#info-close')?.addEventListener('click', () => {
    void close()
  })
}

async function load(): Promise<void> {
  render('<p class="info-empty">正在读取版本信息…</p>')
  try {
    const info = await invoke<DesktopInfo>('get_desktop_info')
    render(renderInfoBody(info))
  } catch (error) {
    render(`<p class="info-error">${escapeHtml(`无法读取版本信息：${String(error)}`)}</p>`)
  }
}

async function close(): Promise<void> {
  try {
    await invoke<boolean>('toggle_desktop_info')
  } catch {
    // The panel is closing anyway; a failure here only means the WebView is
    // already gone.
  }
}

document.addEventListener('keydown', (event) => {
  if (event.key === 'Escape') {
    void close()
  }
})

// The panel is its own document, so it must resolve the Harness theme itself;
// otherwise it keeps using the light design tokens while the rest of the shell
// switches to dark.
void subscribeTheme(() => {
  // Applying `data-theme` restyles the panel through CSS custom properties, so
  // no re-render is needed here.
})

void load()
