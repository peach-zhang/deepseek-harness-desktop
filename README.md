# DSH Desktop

[English](README.md) | [简体中文](README.zh-CN.md)

An unofficial, self-contained Tauri 2 desktop distribution of
[DeepSeek Harness](https://github.com/deepseek-ai/deepseek-harness).

Users download and open a normal Windows or macOS installer. Node.js,
DeepSeek Harness, and its Web UI are already bundled: there is no need to
install Node.js or run `npx @deepseek-ai/dsh web`.

> This project is not affiliated with or endorsed by DeepSeek. DeepSeek and
> DeepSeek Harness are trademarks or projects of their respective owners.

## Download

**Current release: [DSH Desktop v0.1.15](https://github.com/peach-zhang/deepseek-harness-desktop/releases/tag/v0.1.15)**

Windows x64 and macOS installers are available from the
[Latest Release](https://github.com/peach-zhang/deepseek-harness-desktop/releases/latest)
page. Choose the `.exe` installer for Windows, or the DMG matching the Mac's
Apple Silicon (`aarch64`) or Intel (`x64`) architecture. An MSI is also provided
for managed Windows deployments.

> Starting with v0.1.2, macOS installers are signed with Apple Developer ID and
> submitted to Apple for notarization.

DSH Desktop starts a private Harness server on a random `127.0.0.1` port, waits for
its official readiness signal, and opens the built-in Web UI. Closing the app
also stops the Harness process.

## What gets bundled

Versions are deliberately pinned for reproducible releases:

| Component | Version |
| --- | --- |
| DeepSeek Harness | `0.1.0-rc.7` |
| Node.js | `24.19.0` (Krypton LTS) |
| Tauri JavaScript API | `2.11.1` |
| Tauri CLI | `2.11.4` |

The runtime preparation step downloads Node.js directly from `nodejs.org`,
verifies its official SHA-256 checksum, and deploys the locked Harness npm
dependency tree for the build machine's native target. Harness is stored as a
compressed, symlink-preserving archive in the installer and expanded once into
the per-user app-data directory on first launch.

## Runtime auto-update

On every launch DSH Desktop asks the npm registry for the `latest` version of
`@deepseek-ai/dsh`. When a version newer than the current one (bundled or
previously downloaded) exists, it installs that release into the app-data
directory before starting the Harness server; check and download progress is
shown on the launch screen.

A failed update (for example, when offline) never blocks startup — the app
falls back to the runtime bundled in the installer. Installs run with
`--ignore-scripts`, matching how the bundled runtime is built, so no
third-party lifecycle scripts are executed.

The primary registry is the `https://registry.npmmirror.com` mirror. If it
returns inconsistent metadata or an install fails, the updater automatically
retries through the official `https://registry.npmjs.org` registry. Environment
overrides:

| Variable | Effect |
| --- | --- |
| `DSH_DESKTOP_UPDATE_DISABLED=1` | Skip the startup update check entirely |
| `DSH_DESKTOP_REGISTRY=<url>` | Use another primary registry; the official registry remains the fallback |

## Harness plugins

[`src-tauri/plugins/plugins.json`](src-tauri/plugins/plugins.json) lists the
registry plugins enabled by the desktop app. Each entry contains its package
`name` and explicit `command`. On launch, the app validates and runs each
command with the bundled DSH CLI, equivalent to
`dsh plugin --profile web add <name>`. A successful configuration is recorded
so later launches skip installation unless the JSON list changes or an
installed package is missing. Installation failures are logged and never block
startup.

The installer contains only this JSON manifest; plugin package sources are not
stored under `src-tauri/plugins/` or bundled into a separate archive.

## Local development

Requirements for contributors only:

- Node.js 24
- pnpm 10
- Rust stable and the normal Tauri platform prerequisites

```bash
pnpm install
pnpm runtime:prepare
pnpm dev
```

Create a local installer with:

```bash
pnpm build:desktop
```

Generated runtime files live in `src-tauri/runtime/` and are intentionally not
committed.

## Publishing a GitHub Release

1. Push this repository to GitHub with the default branch named `main`.
2. Update the version in `package.json`, `src-tauri/Cargo.toml`, and
   `src-tauri/tauri.conf.json`.
3. Commit and push a matching tag, for example:

```bash
git tag v0.1.15
git push origin v0.1.15
```

The release workflow builds these targets on native GitHub-hosted runners:

- `x86_64-pc-windows-msvc`
- `aarch64-apple-darwin`
- `x86_64-apple-darwin`

It creates a draft GitHub Release while installers are building, then publishes
the release automatically after every platform succeeds. A failed build leaves
the release as a draft so incomplete artifacts are not published.

## Signing

Unsigned builds work, but Windows SmartScreen and macOS Gatekeeper can warn
users. A public production release should be code-signed.

For macOS signing and notarization, configure these repository secrets:

- `APPLE_CERTIFICATE`
- `APPLE_CERTIFICATE_PASSWORD`
- `APPLE_SIGNING_IDENTITY`
- `APPLE_ID`
- `APPLE_PASSWORD`
- `APPLE_TEAM_ID`

Then set the repository variable `ENABLE_APPLE_SIGNING` to `true`. The release
workflow only forwards signing credentials to Tauri when this explicit switch
is enabled. Otherwise it creates unsigned installers, even if stale or partial
Apple secrets exist in the repository.

For Windows, obtain an Authenticode certificate or use Microsoft Trusted
Signing, then add the signing command according to the
[Tauri Windows signing guide](https://v2.tauri.app/distribute/sign/windows/).
Never commit certificates or passwords.

## Updating DeepSeek Harness

Harness is currently a developer preview and may make breaking changes. To
upgrade safely:

1. Change the exact version in `runtime/package.json`.
2. Update the single `HARNESS_VERSION` file at the project root. The Rust
   build script and the frontend constant generator both read from this file,
   so no other source files need editing.
3. Regenerate `runtime/package-lock.json` with `npm install --package-lock-only`
   from the `runtime` directory.
4. Run the desktop smoke test on Windows, Apple Silicon, and Intel macOS.
5. Publish a new wrapper version instead of changing an existing release.

## License

The wrapper is MIT licensed. DeepSeek Harness, Node.js, Tauri, and bundled
dependencies retain their own licenses; see [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md).
