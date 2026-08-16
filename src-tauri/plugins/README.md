# Bundled Harness plugins

Every subdirectory of this folder that contains a `package.json` is treated as
a Cordis plugin and is **installed by default** into the Harness `web` profile
on first launch (and re-synced whenever a bundled plugin's version changes).

If the folder holds no plugin packages, the installer simply ships no extra
plugins; the mechanism stays dormant. (The current bundled plugin is
[`deepseek-usage-plugin/`](deepseek-usage-plugin/) — a sidebar button showing
the DeepSeek account balance.)

## How it works

DSH plugins are Cordis plugin npm packages. A package becomes a *profile
bundle* when its manifest declares a patch layer:

```json
{
  "name": "my-plugin",
  "version": "1.0.0",
  "dependencies": {},
  "dsh": {
    "bundle": {
      "patch": "./cordis.patch.yml"
    }
  }
}
```

The patch file is a top-level YAML array of loader patch entries — the same
format the profile's own `cordis.patch.yml` uses. For a plugin package this is
normally an `insert` block adding the plugin row:

```yaml
# my-plugin/cordis.patch.yml
- insert:
    - id: my-plugin
      name: 'my-plugin'
```

At build time `scripts/prepare-runtime.mjs` packs every such directory into
`src-tauri/runtime/plugins.tar.gz`, which ships inside the installer. On
launch the desktop app (`src-tauri/src/plugins.rs`) copies each package into
`$DSH_HOME/profiles/web/node_modules/`, records it as a dependency, and — when
it declares `dsh.bundle.patch` — appends it to the profile's
`dsh.profile.bundles` layer list, which is exactly what
`dsh plugin --profile web add <package>` does. The bundle layer is applied
after the shipped base bundles, so the plugin is enabled by default.

## Rules for a bundled plugin

- One directory per plugin; the directory name is arbitrary, the npm package
  `name` from `package.json` is authoritative.
- `package.json` **must** have `name` and `version`. Scoped names
  (`@scope/name`) are supported.
- Declaring `dsh.bundle.patch` enables the plugin by default. Without it the
  package is still copied into `node_modules` and recorded as a dependency,
  but it is **not** enabled (the desktop app logs a warning).
- The `dsh.bundle.patch` path must point to a file inside the package.
- A dual-face plugin (host + browser half) declares `dsh.client` and exports
  `./client`; the client-modules service picks it up automatically once the
  entry is enabled — no extra wiring needed (see `deepseek-usage-plugin`).
- Dependencies: rely on in-box `@deepseek-ai/dsh-*` packages (resolved through
  the Harness installation) or vendor third-party packages inside the plugin
  directory. Plain-Node resolution starts at the profile directory, so a
  third-party dependency that is not installed in the Harness runtime must be
  vendored (`node_modules/` inside the plugin directory works).
- Avoid symlinks inside packages on Windows; copying requires no special
  privileges, symlink creation may fail and then the plugin is skipped.
- Keep plugins small — they are unpacked from the archive on every machine
  that installs the desktop app.

## Development

A full installer build bakes `plugins.tar.gz` from this folder automatically
(`pnpm runtime:prepare`). For `tauri dev` (which does not bundle resources),
point the app at this directory directly:

```
DSH_DESKTOP_PLUGINS_DIR=C:\path\to\deepseek-harness-desktop\src-tauri\plugins pnpm tauri dev
```

The installed set is tracked in
`$DSH_HOME/profiles/web/.dsh-desktop-plugin-sync.json`; delete that marker (or
bump a plugin's `version`) to force a re-sync.
