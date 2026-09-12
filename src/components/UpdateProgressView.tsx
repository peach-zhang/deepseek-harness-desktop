import { updateProgress } from '../lib/update-progress'
import type { BackendStatus } from '../types'

/** Step indicator shown while the backend phase is an Harness self-update. */
export function UpdateProgressView({ status }: { status: BackendStatus }) {
  const progress = updateProgress(status)
  if (!progress) return null
  return (
    <div className="update-progress" role="status" aria-live="polite">
      <div className="update-progress__steps">
        {progress.states.map((state, index) => (
          <span
            key={index}
            className={`update-step update-step--${state}`}
            aria-hidden="true"
          />
        ))}
      </div>
      <div className="update-progress__label">
        步骤 {progress.current}/{progress.total}
        {progress.description ? ` · ${progress.description}` : ''}
      </div>
    </div>
  )
}
