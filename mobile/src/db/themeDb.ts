/**
 * Theme persistence. Selection model:
 *
 * - `settings.theme_mode` = 'system' (default) → resolve the builtin theme
 *   matching the OS light/dark scheme.
 * - `settings.theme_mode` = 'explicit' + `settings.selected_theme_id` → use
 *   that theme regardless of OS scheme.
 *
 * Themes are ordered by last_used_at everywhere ("most recently used first").
 * Plugin themes arrive later via upsertTheme() — same table, same flow.
 */

import * as Crypto from 'expo-crypto';

import { getDb } from '@/db/goalsDb';
import type { ThemeMode, ThemeRecord } from '@/theme/types';

export async function listThemes(): Promise<ThemeRecord[]> {
  const db = await getDb();
  return db.getAllAsync<ThemeRecord>(
    'SELECT * FROM themes ORDER BY last_used_at IS NULL, last_used_at DESC, name',
  );
}

async function getSetting(key: string): Promise<string | null> {
  const db = await getDb();
  const row = await db.getFirstAsync<{ value: string }>(
    'SELECT value FROM settings WHERE key = ?',
    [key],
  );
  return row?.value ?? null;
}

async function setSetting(key: string, value: string): Promise<void> {
  const db = await getDb();
  await db.runAsync(
    'INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value',
    [key, value],
  );
}

export async function getThemeState(scheme: 'light' | 'dark'): Promise<{
  mode: ThemeMode;
  active: ThemeRecord;
  themes: ThemeRecord[];
  /** Global per-variable style overrides (`settings.style.<var>`), written
   * by the TUI's `cord.g.style.*` and synced via Turso. Applied above the
   * active theme by themeColorsOf(). */
  styleOverrides: Record<string, string>;
}> {
  const db = await getDb();
  const mode = ((await getSetting('theme_mode')) as ThemeMode | null) ?? 'system';
  const selectedId = await getSetting('selected_theme_id');
  const themes = await listThemes();

  const overrideRows = await db.getAllAsync<{ key: string; value: string }>(
    "SELECT key, value FROM settings WHERE key LIKE 'style.%'",
  );
  const styleOverrides: Record<string, string> = {};
  for (const row of overrideRows) {
    styleOverrides[row.key.slice('style.'.length)] = row.value;
  }

  let active: ThemeRecord | undefined;
  if (mode === 'explicit' && selectedId) {
    active = themes.find((t) => t.id === selectedId);
  }
  if (!active) {
    // System mode (or the explicit selection vanished): match builtin to OS.
    const row = await db.getFirstAsync<ThemeRecord>(
      "SELECT * FROM themes WHERE source = 'builtin' AND id = ? LIMIT 1",
      [scheme === 'dark' ? 'builtin-dark' : 'builtin-light'],
    );
    if (!row) throw new Error('theme-system: builtin themes missing — migration did not run');
    active = row;
  }
  return { mode, active, themes, styleOverrides };
}

/**
 * Pick a theme. Pass null to return to system mode (follows OS light/dark).
 */
export async function selectTheme(id: string | null, scheme?: 'light' | 'dark'): Promise<void> {
  if (id === null) {
    await setSetting('theme_mode', 'system');
    return;
  }
  const db = await getDb();
  await setSetting('theme_mode', 'explicit');
  await setSetting('selected_theme_id', id);
  await db.runAsync('UPDATE themes SET last_used_at = ? WHERE id = ?', [
    new Date().toISOString(),
    id,
  ]);
  void scheme;
}

/**
 * Register/replace a theme. Used by builtins seeding and, later, by plugin
 * installs coming from the TUI.
 */
export async function upsertTheme(input: {
  id?: string;
  name: string;
  source: 'builtin' | 'plugin';
  colorsJson: string;
}): Promise<string> {
  const db = await getDb();
  const id = input.id ?? Crypto.randomUUID();
  await db.runAsync(
    `INSERT INTO themes (id, name, source, colors_json)
     VALUES (?, ?, ?, ?)
     ON CONFLICT(id) DO UPDATE SET
       name = excluded.name,
       source = excluded.source,
       colors_json = excluded.colors_json`,
    [id, input.name, input.source, input.colorsJson],
  );
  return id;
}
