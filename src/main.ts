import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'
import { getCurrentWindow } from '@tauri-apps/api/window'
import './styles.css'

export type BackendPhase = 'starting' | 'checking' | 'updating' | 'running' | 'failed' | 'stopped'

export interface BackendStatus {
  phase: BackendPhase
  message: string
  url: string | null
  harnessVersion: string
}

const root = document.querySelector<HTMLElement>('#app')

if (!root) {
  throw new Error('Missing application root')
}
const appRoot: HTMLElement = root

let retrying = false

const win = getCurrentWindow()

// ── Harness theme sync ──
// The iframe is cross-origin, so the theme preference arrives from the Rust
// side (watching $DSH_HOME/settings.yaml). 'system' is resolved here against
// the OS color scheme.

let harnessThemePreference = 'system'

function applyHarnessTheme(preference: string): void {
  harnessThemePreference = preference
  const systemDark = window.matchMedia('(prefers-color-scheme: dark)').matches
  const dark = preference === 'dark' || (preference === 'system' && systemDark)
  document.documentElement.dataset.theme = dark ? 'dark' : 'light'
}

async function updateMaximizeIcon(): Promise<void> {
  const maximized = await win.isMaximized()
  const iconMax = document.querySelector<SVGElement>('.icon-maximize')
  const iconRestore = document.querySelector<SVGElement>('.icon-restore')
  if (iconMax) iconMax.style.display = maximized ? 'none' : ''
  if (iconRestore) iconRestore.style.display = maximized ? '' : 'none'
}

function escapeHtml(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#039;')
}

function render(status: BackendStatus): void {
  const failed = status.phase === 'failed' || status.phase === 'stopped'
  appRoot.innerHTML = `
    <div class="titlebar" data-tauri-drag-region>
      <span class="titlebar__title" data-tauri-drag-region>DSH Desktop</span>
      <div class="titlebar__controls">
        <button class="titlebar__btn" id="win-minimize" aria-label="最小化">
          <svg width="10" height="1" viewBox="0 0 10 1"><rect width="10" height="1" fill="currentColor"/></svg>
        </button>
        <button class="titlebar__btn" id="win-maximize" aria-label="最大化">
          <svg class="icon-maximize" width="10" height="10" viewBox="0 0 10 10"><rect x="0.5" y="0.5" width="9" height="9" fill="none" stroke="currentColor" stroke-width="1"/></svg>
          <svg class="icon-restore" width="10" height="10" viewBox="0 0 10 10" style="display:none"><rect x="2.5" y="0.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1"/><rect x="0.5" y="2.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1"/></svg>
        </button>
        <button class="titlebar__btn titlebar__btn--close" id="win-close" aria-label="关闭">
          <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1 0 L10 9 M10 0 L1 9" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>
        </button>
      </div>
    </div>
    <section class="shell ${failed ? 'shell--failed' : ''}">
      <div class="card">
        <div class="brand" aria-label="DSH Desktop">
          <div class="mark">
            <svg viewBox="0 0 24 24" xmlns="http://www.w3.org/2000/svg">
              <path d="M12 2C6.48 2 2 6.48 2 12s4.48 10 10 10 10-4.48 10-10S17.52 2 12 2zm-1 14.5v-5H7.5L12.5 5v5H16l-5 6.5z"/>
            </svg>
          </div>
          <div>
            <p class="eyebrow">DESKTOP RUNTIME</p>
            <h1>DSH Desktop</h1>
          </div>
        </div>
        <div class="status-block">
          <div class="spinner ${failed ? 'spinner--failed' : ''}" aria-hidden="true"></div>
          <div>
            <h2>${failed ? '启动遇到问题' : '正在准备工作空间'}</h2>
            <p>${escapeHtml(status.message)}</p>
          </div>
        </div>
        ${failed ? '<button id="retry" type="button">重新启动</button>' : ''}
        <footer>
          <span>Harness ${escapeHtml(status.harnessVersion)}</span>
          <span>Local-only · 127.0.0.1</span>
        </footer>
      </div>
    </section>
  `

  document.querySelector<HTMLButtonElement>('#retry')?.addEventListener('click', () => {
    void restart()
  })

  wireTitlebar()
  void updateMaximizeIcon()
}

function navigateToHarness(url: string): void {
  const parsed = new URL(url)
  if (parsed.protocol !== 'http:' || parsed.hostname !== '127.0.0.1') {
    render({
      phase: 'failed',
      message: '后台返回了不安全的地址，桌面壳已阻止跳转。',
      url: null,
      harnessVersion: '0.1.0-rc.6',
    })
    return
  }
  renderWithIframe(parsed.toString())
}

function renderWithIframe(url: string): void {
  appRoot.innerHTML = `
    <div class="titlebar" data-tauri-drag-region>
      <span class="titlebar__title" data-tauri-drag-region>DSH Desktop</span>
      <div class="titlebar__controls">
        <button class="titlebar__btn" id="win-minimize" aria-label="最小化">
          <svg width="10" height="1" viewBox="0 0 10 1"><rect width="10" height="1" fill="currentColor"/></svg>
        </button>
        <button class="titlebar__btn" id="win-maximize" aria-label="最大化">
          <svg class="icon-maximize" width="10" height="10" viewBox="0 0 10 10"><rect x="0.5" y="0.5" width="9" height="9" fill="none" stroke="currentColor" stroke-width="1"/></svg>
          <svg class="icon-restore" width="10" height="10" viewBox="0 0 10 10" style="display:none"><rect x="2.5" y="0.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1"/><rect x="0.5" y="2.5" width="7" height="7" fill="none" stroke="currentColor" stroke-width="1"/></svg>
        </button>
        <button class="titlebar__btn titlebar__btn--close" id="win-close" aria-label="关闭">
          <svg width="10" height="10" viewBox="0 0 10 10"><path d="M1 0 L10 9 M10 0 L1 9" fill="none" stroke="currentColor" stroke-width="1.2"/></svg>
        </button>
      </div>
    </div>
    <iframe class="harness-frame" src="${escapeHtml(url)}" allow="clipboard-read; clipboard-write"></iframe>
  `

  wireTitlebar()
  void updateMaximizeIcon()
}

function wireTitlebar(): void {
  document.querySelector<HTMLButtonElement>('#win-minimize')?.addEventListener('click', () => {
    void win.minimize()
  })
  document.querySelector<HTMLButtonElement>('#win-maximize')?.addEventListener('click', () => {
    void win.toggleMaximize()
  })
  document.querySelector<HTMLButtonElement>('#win-close')?.addEventListener('click', () => {
    void win.close()
  })
}

function applyStatus(status: BackendStatus): void {
  if (status.phase === 'running' && status.url) {
    navigateToHarness(status.url)
    return
  }
  render(status)
}

async function restart(): Promise<void> {
  if (retrying) return
  retrying = true
  render({
    phase: 'starting',
    message: '正在重新启动内置 Harness…',
    url: null,
    harnessVersion: '0.1.0-rc.6',
  })
  try {
    applyStatus(await invoke<BackendStatus>('restart_backend'))
  } catch (error) {
    render({
      phase: 'failed',
      message: String(error),
      url: null,
      harnessVersion: '0.1.0-rc.6',
    })
  } finally {
    retrying = false
  }
}

async function bootstrap(): Promise<void> {
  render({
    phase: 'starting',
    message: '正在启动内置 Node.js 与 DeepSeek Harness…',
    url: null,
    harnessVersion: '0.1.0-rc.6',
  })

  await listen<BackendStatus>('backend-status', (event) => {
    applyStatus(event.payload)
  })

  await listen<{ preference: string }>('harness-theme', (event) => {
    applyHarnessTheme(event.payload.preference)
  })

  window.matchMedia('(prefers-color-scheme: dark)').addEventListener('change', () => {
    if (harnessThemePreference === 'system') {
      applyHarnessTheme('system')
    }
  })

  try {
    const theme = await invoke<{ preference: string }>('get_harness_theme')
    applyHarnessTheme(theme.preference)
  } catch {
    // Keep the default light theme if the preference cannot be read yet.
  }

  void win.onResized(async () => {
    await updateMaximizeIcon()
  })

  try {
    applyStatus(await invoke<BackendStatus>('backend_status'))
  } catch (error) {
    render({
      phase: 'failed',
      message: String(error),
      url: null,
      harnessVersion: '0.1.0-rc.6',
    })
  }
}

void bootstrap()
