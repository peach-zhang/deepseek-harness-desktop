import { describe, expect, it } from 'vitest'
import { formatTimestamp, renderHistory, renderInfoBody, type DesktopInfo } from './info-utils'

const INFO: DesktopInfo = {
  appVersion: '0.1.15',
  harnessVersion: '0.1.0-rc.7',
  lastUpdateCheck: '2026-09-07T03:40:45Z',
  firstLaunch: null,
  harnessHistory: [
    { version: '0.1.0-rc.7', installTime: '2026-09-07T03:40:45Z', source: 'bundled', isCurrent: true },
    { version: '0.1.0-rc.6', installTime: '2026-09-06T01:02:03Z', source: 'update', isCurrent: false },
  ],
}

describe('版本信息面板渲染', () => {
  it('formats the Rust ISO timestamps and missing values', () => {
    expect(formatTimestamp('2026-09-07T03:40:45Z')).toBe('2026-09-07 03:40 UTC')
    expect(formatTimestamp(null)).toBe('尚未记录')
    expect(formatTimestamp('not-a-timestamp')).toBe('not-a-timestamp')
  })

  it('renders every field the Rust command returns', () => {
    const html = renderInfoBody(INFO)
    expect(html).toContain('v0.1.15')
    expect(html).toContain('0.1.0-rc.7')
    expect(html).toContain('2026-09-07 03:40 UTC')
    expect(html).toContain('尚未记录')
    expect(html).toContain('安装包内置')
    expect(html).toContain('自动更新')
    expect(html.match(/info-badge/g)).toHaveLength(1)
  })

  it('escapes version strings instead of injecting them', () => {
    const html = renderHistory([
      {
        version: '<img src=x onerror=alert(1)>',
        installTime: '2026-09-07T03:40:45Z',
        source: '<script>',
        isCurrent: false,
      },
    ])
    expect(html).not.toContain('<img')
    expect(html).not.toContain('<script>')
    expect(html).toContain('&lt;img')
  })

  it('explains an empty history instead of rendering an empty list', () => {
    expect(renderHistory([])).toContain('暂无版本记录')
    expect(renderHistory([])).not.toContain('<ul')
  })
})
