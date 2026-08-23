/**
 * Theme token definitions — the app's styling constants.
 *
 * Every color used by any component must come from here (via ThemeProvider's
 * useTheme()), never hardcoded in a StyleSheet. Builtin themes are seeded
 * into the DB; plugin-provided themes (installed via the TUI later) carry
 * the same token map and need no code changes.
 */

import type { GoalStatus } from '@/types/goal';

export interface ThemeColors {
  /** Screen background. */ bg: string;
  /** Cards, inputs, inactive tabs. */
  surface: string;
  /** Hairline borders and dividers. */
  border: string;
  /** Tree guide/elbow lines. */
  treeLine: string;
  text: string;
  /** Secondary text. */
  textDim: string;
  /** Placeholders, hints, de-emphasized text. */
  textFaint: string;
  accent: string;
  onAccent: string;
  danger: string;
  statusPending: string;
  statusWip: string;
  statusDone: string;
  statusAgent: string;
}

export const DARK_THEME_COLORS: ThemeColors = {
  bg: '#0f172a',
  surface: '#1e293b',
  border: '#1f2937',
  treeLine: '#334155',
  text: '#f9fafb',
  textDim: '#9ca3af',
  textFaint: '#6b7280',
  accent: '#3b82f6',
  onAccent: '#ffffff',
  danger: '#ef4444',
  statusPending: '#9ca3af',
  statusWip: '#3b82f6',
  statusDone: '#22c55e',
  statusAgent: '#a855f7',
};

export const LIGHT_THEME_COLORS: ThemeColors = {
  bg: '#f8fafc',
  surface: '#ffffff',
  border: '#e2e8f0',
  treeLine: '#cbd5e1',
  text: '#0f172a',
  textDim: '#475569',
  textFaint: '#94a3b8',
  accent: '#2563eb',
  onAccent: '#ffffff',
  danger: '#dc2626',
  statusPending: '#64748b',
  statusWip: '#2563eb',
  statusDone: '#16a34a',
  statusAgent: '#9333ea',
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

export function themeColorsOf(record: ThemeRecord): ThemeColors {
  return { ...DARK_THEME_COLORS, ...JSON.parse(record.colors_json) };
}

/** Fallback while the DB loads so first paint is still styled. */
export const FALLBACK_COLORS = DARK_THEME_COLORS;

/** Status glyph → token mapping, shared by every component that shows status. */
export function statusColor(colors: ThemeColors, status: GoalStatus): string {
  switch (status) {
    case 'pending':
      return colors.statusPending;
    case 'in_progress':
      return colors.statusWip;
    case 'completed':
      return colors.statusDone;
    case 'agent_mode':
      return colors.statusAgent;
  }
}
