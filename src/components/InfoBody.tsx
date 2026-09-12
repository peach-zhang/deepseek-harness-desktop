import { formatTimestamp, type DesktopInfo } from '../lib/desktop-info'

/** Body of the version panel; React escapes every backend-supplied string. */
export function InfoBody({ info }: { info: DesktopInfo }) {
  return (
    <dl className="info-rows">
      <div>
        <dt>DSH Desktop</dt>
        <dd>v{info.appVersion}</dd>
      </div>
      <div>
        <dt>Harness</dt>
        <dd>{info.harnessVersion}</dd>
      </div>
      <div>
        <dt>上次更新检查</dt>
        <dd>{formatTimestamp(info.lastUpdateCheck)}</dd>
      </div>
      <div>
        <dt>首次启动</dt>
        <dd>{formatTimestamp(info.firstLaunch)}</dd>
      </div>
    </dl>
  )
}
