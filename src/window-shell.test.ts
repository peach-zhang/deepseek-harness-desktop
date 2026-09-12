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
    expect(capability.webviews).toEqual(['main', 'info'])
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

  it('关闭按钮隐藏窗口驻留托盘，而不是退出应用', () => {
    // 前端按钮直接隐藏;Alt+F4 等原生关闭请求在 Rust 侧同样被拦截。
    const titlebar = source('./components/Titlebar.tsx')
    expect(titlebar).toContain('win.hide()')
    expect(titlebar).not.toContain('win.close()')
    const app = source('../src-tauri/src/app.rs')
    expect(app).toContain('WindowEvent::CloseRequested { api, .. }')
    expect(app).toContain('api.prevent_close()')
    expect(app).toContain('crate::tray::setup(app.handle())')
    // 托盘提供唤回与真正退出的入口。
    const tray = source('../src-tauri/src/tray.rs')
    expect(tray).toContain('show_main_window')
    expect(tray).toContain('app.exit(0)')
  })

  it('窗口位置与尺寸跨启动保留，但可见性不参与恢复', () => {
    const app = source('../src-tauri/src/app.rs')
    expect(app).toContain('tauri_plugin_window_state')
    expect(app).toContain('window.restore_state(window_state_flags())')
    // 从托盘退出时主窗口是隐藏的;若恢复可见性，下次启动将看不到窗口。
    expect(app).toContain('& !tauri_plugin_window_state::StateFlags::VISIBLE')
  })

  it('首次隐藏到托盘时用系统通知告知去向，且只提示一次', () => {
    const tray = source('../src-tauri/src/tray.rs')
    expect(tray).toContain('pub(crate) fn notify_hidden_once')
    expect(tray).toContain("HINT_KEY")
    expect(source('../src-tauri/src/app.rs')).toContain('crate::tray::notify_hidden_once')
  })
})

describe('版本信息面板契约', () => {
  it('面板作为独立本地 WebView 加载 info.html', () => {
    const commands = source('../src-tauri/src/commands.rs')
    expect(commands).toContain('const INFO_WEBVIEW: &str = "info";')
    expect(commands).toContain('const INFO_PAGE: &str = "info.html";')
    expect(commands).toContain('WebviewBuilder::new(INFO_WEBVIEW, url)')
    expect(commands).toContain('WebviewUrl::App(INFO_PAGE.into())')
    expect(commands).not.toMatch(/WebviewUrl::External/)
  })

  it('两个本地页面都由 Vite 构建为独立入口', () => {
    const config = source('../vite.config.ts')
    expect(config).toContain("resolve(import.meta.dirname, 'index.html')")
    expect(config).toContain("resolve(import.meta.dirname, 'info.html')")
    expect(source('../info.html')).toContain('src="/src/info.tsx"')
    expect(source('../index.html')).toContain('src="/src/main.tsx"')
  })

  it('标题栏入口与面板状态保持同步', () => {
    const main = source('./main.tsx')
    expect(main).toContain("invoke<boolean>('toggle_desktop_info')")
    expect(main).toContain("listen<boolean>('desktop-info'")
    // The info toggle button itself lives in the Titlebar component.
    expect(source('./components/Titlebar.tsx')).toContain('id="win-info"')
  })

  it('面板在 Harness 出现后重建，避免被 z-order 遮挡', () => {
    const backend = source('../src-tauri/src/backend/mod.rs')
    expect(backend).toContain('crate::commands::reopen_info_panel_above_harness(app)')
    expect(backend).toContain('crate::commands::close_info_panel(app)')
  })

  it('面板不绘制原生滚动条，但内容仍可滚动', () => {
    const css = source('./styles.css')
    // The panel is a 340px overlay: a Windows scrollbar covered the values it
    // was meant to reveal, so the bar is suppressed on every scrolling box.
    expect(css).toMatch(
      /body\[data-page='info'\],\s*body\[data-page='info'\] \*\s*\{[^}]*scrollbar-width:\s*none/s,
    )
    expect(css).toContain("-ms-overflow-style: none")
    expect(css).toContain("body[data-page='info'] ::-webkit-scrollbar")
    const body = css.match(/\.info-body\s*\{[^}]*\}/s)?.[0] ?? ''
    expect(body).toContain('overflow-y: auto')
    expect(body).toContain('overscroll-behavior: contain')
  })

  it('面板提供手动检查更新，安装复用后端重启链路', () => {
    const commands = source('../src-tauri/src/commands.rs')
    // 只查询不安装:命令解析 registry 最新版本并刷新 last_update_check。
    expect(commands).toContain('pub(crate) async fn check_harness_update')
    expect(commands).toContain('crate::update::registry_latest_version')
    expect(commands).toContain('db.set_meta("last_update_check"')
    expect(source('../src-tauri/src/app.rs')).toContain('commands::check_harness_update')
    const update = source('./components/UpdateCheck.tsx')
    expect(update).toContain("invoke<UpdateCheckResult>('check_harness_update')")
    // 安装走 restart_backend:start 周期自动下载安装并汇报进度。
    expect(update).toContain("invoke('restart_backend')")
    // 更新需要面板内联二次确认,避免误触直接重启 Harness。
    expect(update).toContain('confirming')
    expect(update).toContain('确认更新')
  })

  it('切换前先采样面板状态，否则关闭后会立刻被重建', () => {
    const commands = source('../src-tauri/src/commands.rs')
    const toggle = commands.slice(commands.indexOf('pub(crate) async fn toggle_desktop_info'))
    const sample = toggle.indexOf('let was_open = is_info_open(&app);')
    const close = toggle.indexOf('close_info_panel(&app)')
    const create = toggle.indexOf('let builder =')
    // Webview::close 会同步注销面板，关闭后再查 get_webview 只会得到 None，
    // 所以打开状态必须在关闭之前采样。
    expect(sample).toBeGreaterThan(-1)
    expect(sample).toBeLessThan(close) 
    expect(close).toBeLessThan(create)
    expect(toggle).toContain('if was_open {')
    expect(toggle).not.toContain('if app.get_webview(INFO_WEBVIEW).is_some()')
  })
})
