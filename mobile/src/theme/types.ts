/**
 * Theme token definitions — the app's styling constants.
 *
 * Every color used by any component must come from here (via ThemeProvider's
 * useTheme()), never hardcoded in a StyleSheet. Builtin themes are seeded
 * into the DB; plugin-provided themes carry the same token map and need no
 * code changes.
 *
 * Tokens use the Compose / Material 3 **role vocabulary**, shared with the
 * TUI (`rust/crates/tui/src/theme.rs`): there are no widget-specific tokens
 * like `statusWip` — statuses consume standard roles (see `statusColor`).
 */

import type { GoalStatus } from '@/types/goal';

export interface ThemeColors {
  /** Screen background. */
  background: string;
  /** Primary text on the background. */
  onBackground: string;
  /** Cards, inputs, inactive tabs. */
  surface: string;
  /** Text/icons on surfaces. */
  onSurface: string;
  /** Muted variant of surface (dividers, wells). */
  surfaceVariant: string;
  /** Secondary text. */
  onSurfaceVariant: string;
  /** Primary action color; also the in-progress status. */
  primary: string;
  /** Text/icons on top of primary. */
  onPrimary: string;
  /** Secondary accent (links, secondary actions). */
  secondary: string;
  /** Text/icons on top of secondary. */
  onSecondary: string;
  /** Third accent; agent-mode status. */
  tertiary: string;
  /** Text/icons on top of tertiary. */
  onTertiary: string;
  /** Completed/success states. */
  success: string;
  /** Text/icons on top of success. */
  onSuccess: string;
  /** Destructive actions / error text. */
  error: string;
  /** Text/icons on top of error. */
  onError: string;
  /** Hairline borders and dividers. */
  outline: string;
  /** Faint lines (tree guides), placeholders, de-emphasized text. */
  outlineVariant: string;
}

export const DARK_THEME_COLORS: ThemeColors = {
  background: '#0f172a',
  onBackground: '#f9fafb',
  surface: '#1e293b',
  onSurface: '#f9fafb',
  surfaceVariant: '#1f2937',
  onSurfaceVariant: '#9ca3af',
  primary: '#3b82f6',
  onPrimary: '#ffffff',
  secondary: '#38bdf8',
  onSecondary: '#082f49',
  tertiary: '#a855f7',
  onTertiary: '#ffffff',
  success: '#22c55e',
  onSuccess: '#052e16',
  error: '#ef4444',
  onError: '#ffffff',
  outline: '#334155',
  outlineVariant: '#6b7280',
};

export const LIGHT_THEME_COLORS: ThemeColors = {
  background: '#f8fafc',
  onBackground: '#0f172a',
  surface: '#ffffff',
  onSurface: '#0f172a',
  surfaceVariant: '#e2e8f0',
  onSurfaceVariant: '#475569',
  primary: '#2563eb',
  onPrimary: '#ffffff',
  secondary: '#0284c7',
  onSecondary: '#ffffff',
  tertiary: '#9333ea',
  onTertiary: '#ffffff',
  success: '#16a34a',
  onSuccess: '#ffffff',
  error: '#dc2626',
  onError: '#ffffff',
  outline: '#cbd5e1',
  outlineVariant: '#94a3b8',
};

export type ThemeSource = 'builtin' | 'plugin';
export type ThemeMode = 'system' | 'explicit';

export interface ThemeRecord {
  id: string;
  name: string;
  source: ThemeSource;
  is_dark: boolean;
  colors_json: string;
  last_used_at: string | null;
}

/**
 * Old token names (pre-role vocabulary) that may still appear in
 * `colors_json` rows — themes authored by earlier plugin versions or older
 * TUI seeds. Applied before canonical keys, so a row carrying both wins
 * with the canonical spelling.
 */
export const LEGACY_ALIASES: Record<string, keyof ThemeColors> = {
  bg: 'background',
  border: 'outline',
  treeLine: 'outlineVariant',
  text: 'onBackground',
  textDim: 'onSurfaceVariant',
  textFaint: 'outlineVariant',
  accent: 'primary',
  onAccent: 'onPrimary',
  danger: 'error',
  statusPending: 'onSurfaceVariant',
  statusWip: 'primary',
  statusDone: 'success',
  statusAgent: 'tertiary',
};

/**
 * Resolve a theme row into tokens. Layering (later wins):
 * builtin dark defaults → row's legacy keys → row's canonical keys →
 * `overrides` (the synced `settings.style.<var>` values written by the
 * TUI's `cord.g.style.*`). Unknown keys are ignored.
 */
export function themeColorsOf(
  record: Pick<ThemeRecord, 'colors_json'>,
  overrides?: Record<string, string>,
): ThemeColors {
  let raw: Record<string, string>;
  try {
    raw = JSON.parse(record.colors_json) ?? {};
  } catch {
    raw = {};
  }
  const colors: ThemeColors = { ...DARK_THEME_COLORS };

  for (const [legacyKey, role] of Object.entries(LEGACY_ALIASES)) {
    const value = raw[legacyKey];
    if (typeof value === 'string') colors[role] = value;
  }
  for (const [key, value] of Object.entries(raw)) {
    if (!(key in LEGACY_ALIASES) && key in colors && typeof value === 'string') {
      colors[key as keyof ThemeColors] = value;
    }
  }
  if (overrides) {
    for (const [key, value] of Object.entries(overrides)) {
      if (key in colors && typeof value === 'string') {
        colors[key as keyof ThemeColors] = value;
      }
    }
  }
  return colors;
}

/** Fallback while the DB loads so first paint is still styled. */
export const FALLBACK_COLORS = DARK_THEME_COLORS;

/** Status glyph → role mapping, shared by every component that shows status. */
export function statusColor(colors: ThemeColors, status: GoalStatus): string {
  switch (status) {
    case 'pending':
      return colors.onSurfaceVariant;
    case 'in_progress':
      return colors.primary;
    case 'completed':
      return colors.success;
    case 'agent_mode':
      return colors.tertiary;
  }
}
