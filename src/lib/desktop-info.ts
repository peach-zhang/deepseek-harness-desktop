export interface DesktopInfo {
  appVersion: string
  harnessVersion: string
  lastUpdateCheck: string | null
  firstLaunch: string | null
}

/** Response of the manual `check_harness_update` command. */
export interface UpdateCheckResult {
  currentVersion: string
  latestVersion: string | null
  upToDate: boolean
  disabled: boolean
}

/** Formats the Rust-side `now_iso()` shape (`YYYY-MM-DDTHH:MM:SSZ`). */
export function formatTimestamp(value: string | null): string {
  if (!value) return '尚未记录'
  const match = /^(\d{4})-(\d{2})-(\d{2})T(\d{2}):(\d{2}):(\d{2})Z$/.exec(value)
  if (!match) return value
  const [, year, month, day, hour, minute] = match
  return `${year}-${month}-${day} ${hour}:${minute} UTC`
}
