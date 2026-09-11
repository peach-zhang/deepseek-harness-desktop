/**
 * Shared theme resolution for the local shell documents.
 *
 * The Harness UI stores its appearance preference server-side; the Rust side
 * polls it and emits `harness-theme`, and `get_harness_theme` returns the
 * current value on demand. Both the bootstrap document and the version panel
 * are *separate* WebViews, each with its own `document`, so each one has to
 * resolve and apply the preference itself: setting `data-theme` in one document
 * has no effect on the other.
 *
 * `styles.css` scopes its dark design tokens to `:root[data-theme='dark']`, so
 * a document that never sets that attribute renders light tokens forever.
 */
import { invoke } from '@tauri-apps/api/core'
import { listen } from '@tauri-apps/api/event'

export const THEME_EVENT = 'harness-theme'
export const THEME_COMMAND = 'get_harness_theme'

/** `system` means "follow the OS", which is resolved against `matchMedia`. */
export type ThemePreference = 'light' | 'dark' | 'system'

export function resolveTheme(preference: string, systemDark: boolean): 'light' | 'dark' {
  return preference === 'dark' || (preference === 'system' && systemDark) ? 'dark' : 'light'
}

/**
 * Applies a preference to the current document's root element.
 * Returns the resolved theme so callers can swap assets that depend on it.
 */
export function applyTheme(preference: string): 'light' | 'dark' {
  const theme = resolveTheme(
    preference,
    window.matchMedia('(prefers-color-scheme: dark)').matches,
  )
  document.documentElement.dataset.theme = theme
  return theme
}

/**
 * Subscribes the current document to theme changes: the initial preference from
 * the Rust side, live `harness-theme` events, and OS changes while the
 * preference is `system`.
 *
 * `onApplied` runs for every applied theme, including the initial one.
 * Returns an unsubscribe function that also detaches the media-query listener.
 */
export async function subscribeTheme(
  onApplied: (theme: 'light' | 'dark') => void,
): Promise<() => void> {
  let preference: ThemePreference = 'system'

  const apply = (next: string): void => {
    preference = next === 'dark' || next === 'light' ? next : 'system'
    onApplied(applyTheme(preference))
  }

  const media = window.matchMedia('(prefers-color-scheme: dark)')
  const onSystemChange = (): void => {
    if (preference === 'system') {
      apply('system')
    }
  }
  media.addEventListener('change', onSystemChange)

  // Subscribe before fetching, so a change that lands during the fetch is not
  // missed (`main.ts` historically applied them in this order too).
  await listen<{ preference: string }>(THEME_EVENT, (event) => {
    apply(event.payload.preference)
  })

  try {
    const theme = await invoke<{ preference: string }>(THEME_COMMAND)
    apply(theme.preference)
  } catch {
    // Keep the current theme if the preference cannot be read yet; the next
    // `harness-theme` event will correct it.
  }

  return () => {
    media.removeEventListener('change', onSystemChange)
  }
}
