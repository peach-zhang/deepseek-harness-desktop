export interface UpdateProgressInput {
  updateStage?: number
  updateStageTotal?: number
  updateStageDescription?: string
}

export interface UpdateProgress {
  current: number
  total: number
  description: string
  states: Array<'done' | 'active' | 'pending'>
}

export function escapeHtml(value: string): string {
  return value
    .replaceAll('&', '&amp;')
    .replaceAll('<', '&lt;')
    .replaceAll('>', '&gt;')
    .replaceAll('"', '&quot;')
    .replaceAll("'", '&#039;')
}

export function updateProgress(input: UpdateProgressInput): UpdateProgress | null {
  const current = input.updateStage
  const total = input.updateStageTotal
  if (!current || !total || current < 1 || current > total) return null
  return {
    current,
    total,
    description: input.updateStageDescription ?? '',
    states: Array.from({ length: total }, (_, index) => {
      const step = index + 1
      return step < current ? 'done' : step === current ? 'active' : 'pending'
    }),
  }
}
