import { BrandWordmark } from './BrandWordmark'
import { UpdateProgressView } from './UpdateProgressView'
import type { BackendStatus } from '../types'

interface StatusCardProps {
  status: BackendStatus
  onRetry: () => void
}

export function StatusCard({ status, onRetry }: StatusCardProps) {
  const failed = status.phase === 'failed' || status.phase === 'stopped'
  return (
    <section className={`shell${failed ? ' shell--failed' : ''}`}>
      <div className="card">
        <div className="brand" aria-label="DSH Desktop">
          <div>
            <h1 className="wordmark" aria-label="DSH Desktop">
              <BrandWordmark />
            </h1>
          </div>
        </div>
        <div className="status-block">
          <div className={`spinner${failed ? ' spinner--failed' : ''}`} aria-hidden="true" />
          <div>
            <h2>{failed ? '启动遇到问题' : '正在准备工作空间'}</h2>
            <p>{status.message}</p>
          </div>
        </div>
        <UpdateProgressView status={status} />
        {failed ? (
          <button id="retry" type="button" onClick={onRetry}>
            重新启动
          </button>
        ) : null}
        <footer>
          <span>Harness {status.harnessVersion}</span>
          <span>Local-only · 127.0.0.1</span>
        </footer>
      </div>
    </section>
  )
}
