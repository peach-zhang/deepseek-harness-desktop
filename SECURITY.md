# Security policy

## Supported versions

Only the latest GitHub Release is supported. The installer includes a reviewed
Harness fallback, while the desktop supervisor can install a newer signed npm
runtime and quarantine it automatically if startup fails.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting feature. Do not open a
public issue containing API keys, private source code, logs with credentials,
or an unpatched exploit.

## Desktop security boundary

- The Harness server binds only to `127.0.0.1` on an operating-system-selected
  port.
- 仅将通过校验的 loopback readiness URL 作为独立 Harness 子 WebView 的顶层文档加载，
  让 DSH 使用第一方会话 Cookie 完成启动 token 交换；本地标题栏始终保留。
- The launch token is never sent through frontend IPC and is redacted from logs.
- 本地壳层只能导航到 bootstrap origin，Harness 子 WebView 只能导航到当前活动的
  `127.0.0.1:<port>` origin，两者不能互相跳转。
- Tauri IPC capability 仅授予本地 `main` 与 `info` 两个 WebView（启动壳层与版本信息面板），
  不按整个窗口授权；Harness 子 WebView 及远程 origin 均不获得 Tauri IPC 权限。
- Harness and model data live under the operating system's per-user app-data
  directory.
- Telemetry is disabled by the wrapper unless a future release exposes an
  explicit user setting.

## Content Security Policy

The CSP configured in `src-tauri/tauri.conf.json` restricts resource loading:

- **`default-src 'self'`**: Only load resources from the app itself by default.
- **`connect-src 'self' ipc: http://ipc.localhost`**: Allow connections to the
  app and Tauri's IPC endpoints.
- **`img-src 'self' data:`**: Allow images from the app and data URIs (used for
  inline SVG icons and theme assets).
- **`style-src 'self' 'unsafe-inline'`**: Allow styles from the app and inline
  styles. The `'unsafe-inline'` directive is required because Vite injects
  styles during development and some CSS frameworks may generate inline styles.
  In production, if all styles are bundled by Vite into external CSS files,
  this directive could be removed after testing.
- **`script-src 'self'`**: Only allow scripts from the app itself (no external
  or inline scripts).

Harness UI 在标题栏下方的独立原生 WebView 中加载，不替换本地 bootstrap 文档，
也不使用 iframe，因此 bootstrap CSP 无需开放到 loopback 服务的 iframe 访问。
