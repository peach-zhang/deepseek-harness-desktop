import { describe, expect, it } from 'vitest'
import { escapeHtml, updateProgress } from './bootstrap-utils'

describe('desktop bootstrap contract', () => {
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
