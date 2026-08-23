# Security policy

## Supported versions

Only the latest GitHub Release is supported. DeepSeek Harness is currently a
developer preview, so this wrapper pins one reviewed Harness version per
release instead of silently updating it at runtime.

## Reporting a vulnerability

Please use GitHub's private vulnerability reporting feature. Do not open a
public issue containing API keys, private source code, logs with credentials,
or an unpatched exploit.

## Desktop security boundary

- The Harness server binds only to `127.0.0.1` on an operating-system-selected
  port.
- Only a validated loopback readiness URL is loaded.
- The remote Harness origin is not granted Tauri IPC permissions.
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
- **`frame-src http://127.0.0.1:*`**: Allow iframes from the local Harness
  server (bound to loopback on a random port).
