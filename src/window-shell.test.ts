import { readFileSync } from 'node:fs'
import { describe, expect, it } from 'vitest'

const source = (path: string) => readFileSync(new URL(path, import.meta.url), 'utf8')

describe('自定义窗口壳层契约', () => {
  it('主窗口始终使用自定义边框', () => {
    const config = JSON.parse(source('../src-tauri/tauri.conf.json'))
    expect(config.app.windows.find((window: { label: string }) => window.label === 'main').decorations).toBe(false)
    expect(source('../src-tauri/src/backend/mod.rs')).not.toMatch(/set_decorations\(true\)/)
  })

  it('仅本地壳层 WebView 获得 IPC 权限，而不是整个窗口', () => {
    const capability = JSON.parse(source('../src-tauri/capabilities/default.json'))
    expect(capability.webviews).toEqual(['main'])
    expect(capability.windows).toBeUndefined()
    expect(capability.local).toBe(true)
    expect(capability.remote).toBeUndefined()
    expect(capability.permissions).toContain('core:window:allow-start-dragging')
    expect(capability.permissions).toContain('core:window:allow-close')
  })

  it('原生内容区域与 CSS 标题栏高度保持一致', () => {
    const css = source('./styles.css')
    const layout = source('../src-tauri/src/window_shell.rs')
    const titlebarHeight = css.match(/\.titlebar\s*\{[^}]*height:\s*(\d+)px/s)?.[1]
    expect(titlebarHeight).toBeDefined()
    expect(layout).toContain(`const TITLEBAR_HEIGHT: f64 = ${titlebarHeight}.0;`)
  })

  it('Harness 使用独立 WebView，不替换本地标题栏文档', () => {
    const backend = source('../src-tauri/src/backend/mod.rs')
    expect(backend).not.toMatch(/window\.navigate\(/)
    expect(backend).toContain('WebviewBuilder::new("harness", WebviewUrl::External(url))')
    expect(backend).toContain('allows_harness_navigation(url)')
    expect(source('../src-tauri/src/app.rs')).toContain('allows_bootstrap_navigation(url)')
  })
})
