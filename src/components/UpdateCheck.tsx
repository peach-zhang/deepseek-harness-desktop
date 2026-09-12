import { useState } from 'react'
import { invoke } from '@tauri-apps/api/core'
import { ArrowUpCircle, Check, RefreshCw } from 'lucide-react'
import type { UpdateCheckResult } from '../lib/desktop-info'

type CheckState =
  | { kind: 'idle' }
  | { kind: 'checking' }
  | { kind: 'done'; result: UpdateCheckResult; confirming: boolean }
  | { kind: 'error'; message: string }

/**
 * Manual Harness update check. The command only inspects the registry; when a
 * newer version exists, the install asks for an inline confirmation and then
 * reuses `restart_backend`, whose start cycle downloads and stages the runtime
 * — and the bootstrap restore closes this panel while the main window shows
 * the update progress.
 */
export function UpdateCheck({ onChecked }: { onChecked: () => void }) {
  const [state, setState] = useState<CheckState>({ kind: 'idle' })

  const check = () => {
    setState({ kind: 'checking' })
    invoke<UpdateCheckResult>('check_harness_update')
      .then((result) => {
        setState({ kind: 'done', result, confirming: false })
        // A real check refreshes the "上次更新检查" timestamp shown above.
        if (!result.disabled) onChecked()
      })
      .catch((error: unknown) => {
        setState({ kind: 'error', message: String(error) })
      })
  }

  const install = () => {
    void invoke('restart_backend').catch((error: unknown) => {
      setState({ kind: 'error', message: String(error) })
    })
  }

  const checking = state.kind === 'checking'
  const found = state.kind === 'done' && !state.result.disabled && !state.result.upToDate
  const confirming = state.kind === 'done' && state.confirming

  return (
    <div className="info-update">
      {state.kind === 'done' && state.result.disabled ? (
        <span className="info-update__result">更新检查已被禁用（DSH_DESKTOP_UPDATE_DISABLED）</span>
      ) : state.kind === 'done' && state.result.upToDate ? (
        <span className="info-update__result info-update__result--ok">
          <Check size={12} aria-hidden="true" />
          {state.result.latestVersion === state.result.currentVersion
            ? `已是最新版本 v${state.result.currentVersion}`
            : `当前 v${state.result.currentVersion} 已不低于最新发布 v${state.result.latestVersion}`}
        </span>
      ) : state.kind === 'done' ? (
        <span className="info-update__result info-update__result--new">
          <ArrowUpCircle size={12} aria-hidden="true" />
          {confirming
            ? `确认更新到 v${state.result.latestVersion}？重启期间界面会暂时不可用`
            : `发现新版本 v${state.result.latestVersion}（当前 v${state.result.currentVersion}）`}
        </span>
      ) : state.kind === 'error' ? (
        <span className="info-update__result info-update__result--error">{state.message}</span>
      ) : null}
      {found && confirming ? (
        <>
          <button
            className="info-update__btn info-update__btn--primary"
            type="button"
            onClick={install}
          >
            确认更新
          </button>
          <button
            className="info-update__btn"
            type="button"
            onClick={() =>
              setState(state.kind === 'done' ? { ...state, confirming: false } : state)
            }
          >
            取消
          </button>
        </>
      ) : found ? (
        <button
          className="info-update__btn info-update__btn--primary"
          type="button"
          onClick={() =>
            setState(state.kind === 'done' ? { ...state, confirming: true } : state)
          }
        >
          立即更新
        </button>
      ) : (
        <button
          className="info-update__btn"
          type="button"
          disabled={checking}
          onClick={check}
        >
          <RefreshCw
            size={12}
            aria-hidden="true"
            className={checking ? 'info-update__spin' : undefined}
          />
          {checking ? '检查中…' : '检查更新'}
        </button>
      )}
    </div>
  )
}
