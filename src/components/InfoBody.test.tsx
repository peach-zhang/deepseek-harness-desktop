import { describe, expect, it } from 'vitest'
import { renderToStaticMarkup } from 'react-dom/server'
import { InfoBody } from './InfoBody'
import { formatTimestamp, type DesktopInfo } from '../lib/desktop-info'

const INFO: DesktopInfo = {
  appVersion: '0.1.15',
  harnessVersion: '0.1.0-rc.7',
  lastUpdateCheck: '2026-09-07T03:40:45Z',
  firstLaunch: null,
}

describe('版本信息面板渲染', () => {
  it('formats the Rust ISO timestamps and missing values', () => {
    expect(formatTimestamp('2026-09-07T03:40:45Z')).toBe('2026-09-07 03:40 UTC')
    expect(formatTimestamp(null)).toBe('尚未记录')
    expect(formatTimestamp('not-a-timestamp')).toBe('not-a-timestamp')
  })

  it('renders every field the Rust command returns', () => {
    const html = renderToStaticMarkup(<InfoBody info={INFO} />)
    expect(html).toContain('v0.1.15')
    expect(html).toContain('0.1.0-rc.7')
    expect(html).toContain('2026-09-07 03:40 UTC')
    expect(html).toContain('尚未记录')
  })

  it('escapes version strings instead of injecting them', () => {
    const html = renderToStaticMarkup(
      <InfoBody
        info={{
          ...INFO,
          appVersion: '<img src=x onerror=alert(1)>',
          harnessVersion: '<script>',
        }}
      />,
    )
    expect(html).not.toContain('<img')
    expect(html).not.toContain('<script>')
    expect(html).toContain('&lt;img')
  })
})
