import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest'
import { readFileSync } from 'node:fs'

const source = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8')

// `./theme` touches exactly two browser globals: `window.matchMedia` and
// `document.documentElement.dataset`. Stubbing just those keeps this suite free
// of a DOM dependency (the repo ships neither jsdom nor happy-dom) and makes the
// assertions exact rather than DOM-implementation dependent.
interface SystemColorScheme {
  setSystemDark(next: boolean): void
  listenerCount(): number
}

function installDomStubs(initiallyDark: boolean): SystemColorScheme {
  const listeners = new Set<() => void>()
  let dark = initiallyDark
  const dataset: Record<string, string> = {}

  vi.stubGlobal('document', { documentElement: { dataset } })
  vi.stubGlobal('window', {
    matchMedia: (query: string) => ({
      get matches() {
        return dark
      },
      media: query,
      addEventListener: (_: string, listener: () => void) => listeners.add(listener),
      removeEventListener: (_: string, listener: () => void) => listeners.delete(listener),
    }),
  })

  return {
    setSystemDark(next: boolean) {
      dark = next
      listeners.forEach((listener) => listener())
    },
    listenerCount: () => listeners.size,
  }
}

function currentTheme(): string | undefined {
  return (document.documentElement as unknown as { dataset: Record<string, string> }).dataset
    .theme
}

describe('主题解析', () => {
  afterEach(() => {
    vi.unstubAllGlobals()
    vi.resetModules()
  })

  it('按偏好与系统配色解析出主题', async () => {
    const { resolveTheme } = await import('./theme')

    expect(resolveTheme('dark', false)).toBe('dark')
    expect(resolveTheme('light', true)).toBe('light')
    // `system` is the only preference that consults the OS setting.
    expect(resolveTheme('system', true)).toBe('dark')
    expect(resolveTheme('system', false)).toBe('light')
    // A missing or unknown preference must not force dark.
    expect(resolveTheme('', true)).toBe('light')
    expect(resolveTheme('neon', true)).toBe('light')
  })
})

describe('本地文档主题订阅', () => {
  let system: SystemColorScheme
  let eventListeners: Map<string, (event: { payload: { preference: string } }) => void>
  let preference: string

  beforeEach(() => {
    system = installDomStubs(false)
    eventListeners = new Map()
    preference = 'system'

    vi.doMock('@tauri-apps/api/core', () => ({
      invoke: vi.fn(async (command: string) => {
        if (command !== 'get_harness_theme') {
          throw new Error(`unexpected command: ${command}`)
        }
        return { preference }
      }),
    }))
    vi.doMock('@tauri-apps/api/event', () => ({
      listen: vi.fn(
        async (event: string, handler: (event: { payload: { preference: string } }) => void) => {
          eventListeners.set(event, handler)
          return () => eventListeners.delete(event)
        },
      ),
    }))
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    vi.doUnmock('@tauri-apps/api/core')
    vi.doUnmock('@tauri-apps/api/event')
    vi.resetModules()
  })

  it('订阅时即应用 Rust 返回的偏好', async () => {
    preference = 'dark'
    const { subscribeTheme } = await import('./theme')
    const applied: string[] = []

    await subscribeTheme((theme) => applied.push(theme))

    expect(applied).toEqual(['dark'])
    expect(currentTheme()).toBe('dark')
  })

  it('跟随 harness-theme 事件切换主题', async () => {
    const { subscribeTheme, THEME_EVENT } = await import('./theme')
    await subscribeTheme(() => {})

    const handler = eventListeners.get(THEME_EVENT)
    expect(handler, '面板必须监听 harness-theme 事件').toBeDefined()

    handler!({ payload: { preference: 'dark' } })
    expect(currentTheme()).toBe('dark')

    handler!({ payload: { preference: 'light' } })
    expect(currentTheme()).toBe('light')
  })

  it('仅在偏好为 system 时跟随系统配色', async () => {
    preference = 'system'
    const { subscribeTheme, THEME_EVENT } = await import('./theme')
    await subscribeTheme(() => {})
    expect(currentTheme()).toBe('light')

    system.setSystemDark(true)
    expect(currentTheme()).toBe('dark')

    // An explicit preference must win over the OS setting.
    eventListeners.get(THEME_EVENT)!({ payload: { preference: 'light' } })
    system.setSystemDark(false)
    system.setSystemDark(true)
    expect(currentTheme()).toBe('light')
  })

  it('未知偏好回退为 system 而不是留在上一次的取值', async () => {
    const { subscribeTheme, THEME_EVENT } = await import('./theme')
    await subscribeTheme(() => {})

    eventListeners.get(THEME_EVENT)!({ payload: { preference: 'neon' } })
    // `system` on a light OS resolves to light, so the panel must not stay dark.
    expect(currentTheme()).toBe('light')

    system.setSystemDark(true)
    expect(currentTheme()).toBe('dark')
  })

  it('退订时移除系统配色监听', async () => {
    const { subscribeTheme } = await import('./theme')
    const unsubscribe = await subscribeTheme(() => {})
    expect(system.listenerCount()).toBe(1)

    unsubscribe()
    expect(system.listenerCount()).toBe(0)
  })

  it('偏好读取失败时不设置主题，也不抛错', async () => {
    vi.doMock('@tauri-apps/api/core', () => ({
      invoke: vi.fn(async () => {
        throw new Error('backend not ready')
      }),
    }))
    const { subscribeTheme } = await import('./theme')
    await expect(subscribeTheme(() => {})).resolves.toBeTypeOf('function')

    // The stylesheet's light tokens apply by default, so leaving `data-theme`
    // unset is the correct fallback rather than guessing a theme.
    expect(currentTheme()).toBeUndefined()
  })
})

describe('两个本地文档都接入共享主题模块', () => {
  it('版本信息面板导入并订阅主题', () => {
    const info = source('./info.tsx')
    // Regression guard: the panel is its own document, so without this it kept
    // the light design tokens while the rest of the shell switched to dark.
    expect(info).toContain("from './theme'")
    expect(info).toContain('subscribeTheme(')
  })

  it('主窗口改用共享模块，不再自带重复实现', () => {
    const main = source('./main.tsx')
    expect(main).toContain("from './theme'")
    expect(main).toContain('subscribeTheme(')
    // The old local copy duplicated the resolution and the event wiring.
    expect(main).not.toContain('function applyHarnessTheme')
    expect(main).not.toContain("listen<{ preference: string }>('harness-theme'")
  })

  it('暗色 token 以 data-theme 为开关，两个文档都必须设置它', () => {
    expect(source('./styles.css')).toContain(":root[data-theme='dark']")
    expect(source('./theme.ts')).toContain('document.documentElement.dataset.theme')
  })
})
