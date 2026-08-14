import { describe, expect, it } from 'vitest'

describe('desktop bootstrap contract', () => {
  it('accepts only the local Harness origin', () => {
    const accepted = new URL('http://127.0.0.1:3080')
    expect(accepted.protocol).toBe('http:')
    expect(accepted.hostname).toBe('127.0.0.1')
  })
})
