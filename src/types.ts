/** Lifecycle of the embedded Harness backend, reported by the Rust side. */
export type BackendPhase = 'starting' | 'checking' | 'updating' | 'running' | 'failed' | 'stopped'

export interface BackendStatus {
  phase: BackendPhase
  message: string
  harnessVersion: string
  updateStage?: number
  updateStageTotal?: number
  updateStageDescription?: string
}
