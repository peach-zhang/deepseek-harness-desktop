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
