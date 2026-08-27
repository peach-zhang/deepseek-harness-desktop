import { describe, expect, it } from 'vitest'
import { escapeHtml, parseHarnessUrl, updateProgress } from './bootstrap-utils'

describe('desktop bootstrap contract', () => {
  it('accepts only explicit local Harness HTTP endpoints', () => {
    expect(parseHarnessUrl('http://127.0.0.1:3080')?.toString()).toBe('http://127.0.0.1:3080/')
    expect(parseHarnessUrl('http://127.0.0.1')).toBeNull()
    expect(parseHarnessUrl('https://127.0.0.1:3080')).toBeNull()
    expect(parseHarnessUrl('http://localhost:3080')).toBeNull()
    expect(parseHarnessUrl('http://user@127.0.0.1:3080')).toBeNull()
    expect(parseHarnessUrl('not a url')).toBeNull()
  })

  it('escapes status text before inserting it into HTML', () => {
    expect(escapeHtml(`<script data-x="1">'unsafe' & text</script>`)).toBe(
      '&lt;script data-x=&quot;1&quot;&gt;&#039;unsafe&#039; &amp; text&lt;/script&gt;',
    )
  })

  it('builds stable update progress states and rejects invalid ranges', () => {
    expect(updateProgress({ updateStage: 2, updateStageTotal: 4, updateStageDescription: '安装' })).toEqual({
      current: 2,
      total: 4,
      description: '安装',
      states: ['done', 'active', 'pending', 'pending'],
    })
    expect(updateProgress({ updateStage: 0, updateStageTotal: 4 })).toBeNull()
    expect(updateProgress({ updateStage: 5, updateStageTotal: 4 })).toBeNull()
  })
})
