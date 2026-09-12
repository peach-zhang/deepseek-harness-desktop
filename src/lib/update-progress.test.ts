import { describe, expect, it } from 'vitest'
import { updateProgress } from './update-progress'

describe('desktop bootstrap contract', () => {
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
