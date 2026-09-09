/**
 * Pure rendering helpers for the version panel.
 *
 * Kept separate from `info.ts` so the formatting and escaping rules can be
 * unit tested without a DOM or a running Tauri host.
 */
import { escapeHtml } from './bootstrap-utils'

export interface HarnessHistoryEntry {
  version: string
  installTime: string
  source: string
  isCurrent: boolean
}

export interface DesktopInfo {
  appVersion: string
  harnessVersion: string
  lastUpdateCheck: string | null
  firstLaunch: string | null
  harnessHistory: HarnessHistoryEntry[]
}

export const SOURCE_LABELS: Record<string, string> = {
  bundled: '安装包内置',
  update: '自动更新',
}

/** Renders the Rust-side `now_iso()` shape (`YYYY-MM-DDTHH:MM:SSZ`). */
export function formatTimestamp(value: string | null): string {
  if (!value) return '尚未记录'
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})Z$/.exec(value)
  if (!match) return value
  const [, year, month, day, hour, minute] = match
  return `${year}-${month}-${day} ${hour}:${minute} UTC`
}

export function renderHistory(entries: HarnessHistoryEntry[]): string {
  if (entries.length === 0) {
    return '<p class="info-empty">暂无版本记录。</p>'
  }
  const rows = entries
    .map((entry) => {
      const source = SOURCE_LABELS[entry.source] ?? entry.source
      const badge = entry.isCurrent ? '<span class="info-badge">当前</span>' : ''
      return `
        <li class="info-history__item">
          <div class="info-history__head">
            <code>${escapeHtml(entry.version)}</code>
            ${badge}
          </div>
          <div class="info-history__meta">
            <span>${escapeHtml(source)}</span>
            <span>${escapeHtml(formatTimestamp(entry.installTime))}</span>
          </div>
        </li>
      `
    })
    .join('')
  return `<ul class="info-history">${rows}</ul>`
}

export function renderInfoBody(info: DesktopInfo): string {
  return `
    <dl class="info-rows">
      <div><dt>DSH Desktop</dt><dd>v${escapeHtml(info.appVersion)}</dd></div>
      <div><dt>Harness</dt><dd>${escapeHtml(info.harnessVersion)}</dd></div>
      <div><dt>上次更新检查</dt><dd>${escapeHtml(formatTimestamp(info.lastUpdateCheck))}</dd></div>
      <div><dt>首次启动</dt><dd>${escapeHtml(formatTimestamp(info.firstLaunch))}</dd></div>
    </dl>
    <h3 class="info-heading">版本历史</h3>
    ${renderHistory(info.harnessHistory)}
  `
}
