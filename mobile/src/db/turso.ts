/**
 * Turso sync — pulls and pushes shared data over the Turso HTTP pipeline
 * API (Hrana over HTTP), no extra dependencies.
 *
 * Model (mirrors the TUI's last-write-wins contract):
 * - **goals**: pulled fully and merged by `updated_at` (newer wins), pushed
 *   incrementally (rows newer than the last push). Deletes do NOT
 *   propagate — the schema has no tombstones.
 * - **settings**: pulled only (the TUI is the source of truth for
 *   `style.*` overrides, plugin config, ...). Turso credentials and sync
 *   bookkeeping keys are excluded and never leave the device.
 * - **themes**: pulled only; `is_dark` is derived from the background
 *   color's luminance (the TUI's schema has no is_dark column).
 *
 * Credentials live in the local `settings` table under `turso_url` /
 * `turso_token`. They are device-local: the pull excludes them, and
 * settings are never pushed, so they never reach the remote.
 */

import { getDb } from '@/db/goalsDb';
import { logError } from '@/db/errorsDb';

export interface TursoCreds {
  url: string;
  token: string;
}

const CREDS_URL_KEY = 'turso_url';
const CREDS_TOKEN_KEY = 'turso_token';
const LAST_PULL_KEY = 'sync.last_pull';
const LAST_PUSH_KEY = 'sync.last_push';

/** Settings keys that never leave the device and are never overwritten
 * by a pull. */
function isLocalOnlyKey(key: string): boolean {
  return key === CREDS_URL_KEY || key === CREDS_TOKEN_KEY || key.startsWith('sync.');
}

export async function getTursoCreds(): Promise<TursoCreds | null> {
  const db = await getDb();
  const rows = await db.getAllAsync<{ key: string; value: string }>(
    "SELECT key, value FROM settings WHERE key IN ('turso_url', 'turso_token')",
  );
  let url: string | undefined;
  let token: string | undefined;
  for (const row of rows) {
    if (row.key === CREDS_URL_KEY) url = row.value;
    if (row.key === CREDS_TOKEN_KEY) token = row.value;
  }
  if (!url || !token) return null;
  return { url, token };
}

export async function setTursoCreds(creds: TursoCreds): Promise<void> {
  const db = await getDb();
  for (const [key, value] of [
    [CREDS_URL_KEY, creds.url],
    [CREDS_TOKEN_KEY, creds.token],
  ] as const) {
    await db.runAsync(
      'INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value',
      [key, value],
    );
  }
}

export async function isSyncConfigured(): Promise<boolean> {
  return (await getTursoCreds()) !== null;
}

async function getMeta(key: string): Promise<string | null> {
  const db = await getDb();
  const row = await db.getFirstAsync<{ value: string }>(
    'SELECT value FROM settings WHERE key = ?',
    [key],
  );
  return row?.value ?? null;
}

async function setMeta(key: string, value: string): Promise<void> {
  const db = await getDb();
  await db.runAsync(
    'INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value',
    [key, value],
  );
}

export function formatLastSync(iso: string | null): string {
  if (!iso) return 'never';
  const secs = Math.max(0, Math.floor((Date.now() - Date.parse(iso)) / 1000));
  if (secs < 60) return 'just now';
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  return `${Math.floor(secs / 3600)}h ago`;
}

export async function lastSyncedAt(): Promise<string | null> {
  return getMeta(LAST_PULL_KEY);
}

// ---------- minimal Hrana-over-HTTP client ----------

type HranaArg = { type: 'text'; value: string } | { type: 'integer'; value: number };

interface HranaRow {
  type: string;
  value: unknown;
}

interface HranaResult {
  rows: HranaRow[][];
  cols: string[];
}

/** Execute a batch of statements against the Turso HTTP pipeline API. */
async function pipeline(
  creds: TursoCreds,
  stmts: { sql: string; args?: HranaArg[] }[],
): Promise<HranaResult[]> {
  const endpoint = creds.url.replace(/\/+$/, '') + '/v2/pipeline';
  const body = {
    requests: [
      ...stmts.map((s) => ({
        type: 'execute',
        stmt: { sql: s.sql, args: s.args ?? [] },
      })),
      { type: 'close' },
    ],
  };
  const response = await fetch(endpoint, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      authorization: `Bearer ${creds.token}`,
    },
    body: JSON.stringify(body),
  });
  if (!response.ok) {
    throw new Error(`turso http ${response.status}`);
  }
  const json = (await response.json()) as {
    results: (
      | { type: 'ok'; response: { results: HranaResult[] } }
      | { type: 'error'; error?: unknown }
    )[];
  };
  const out: HranaResult[] = [];
  for (const result of json.results ?? []) {
    if (result.type !== 'ok') {
      throw new Error(`turso stmt failed: ${JSON.stringify(result).slice(0, 200)}`);
    }
    out.push(...result.response.results);
  }
  return out;
}

function toHranaArgs(args: (string | number)[]): HranaArg[] {
  return args.map((a) =>
    typeof a === 'number'
      ? { type: 'integer' as const, value: a }
      : { type: 'text' as const, value: a },
  );
}

function rowsToObjects(result: HranaResult): Record<string, unknown>[] {
  return result.rows.map((row) => {
    const obj: Record<string, unknown> = {};
    result.cols.forEach((col, i) => {
      const cell = row[i];
      obj[col] = cell?.value;
    });
    return obj;
  });
}

// ---------- sync ----------

export interface SyncOutcome {
  ok: boolean;
  /** What happened, human-readable for the status line. */
  message: string;
  pulledGoals: number;
  pushedGoals: number;
}

const GOAL_COLS =
  'id, title, description, status, parent_id, sheet_id, sort_order, created_at, updated_at, completed_at';

/** Pull remote goals + settings + themes, push dirty local goals. */
export async function syncNow(): Promise<SyncOutcome> {
  let pulledGoals = 0;
  let pushedGoals = 0;

  try {
    const creds = await getTursoCreds();
    if (!creds) {
      return { ok: false, message: 'not configured', pulledGoals, pushedGoals };
    }

    const db = await getDb();
    // --- goals: pull everything and merge LWW by updated_at ---
    const lastPull = (await getMeta(LAST_PULL_KEY)) ?? '';
    const results = await pipeline(creds, [
      {
        sql: `SELECT ${GOAL_COLS} FROM goals WHERE updated_at > ?`,
        args: toHranaArgs([lastPull]),
      },
    ]);
    const remoteGoals = rowsToObjects(results[0]);
    for (const g of remoteGoals) {
      const id = String(g.id);
      const remoteUpdatedAt = String(g.updated_at);
      const local = await db.getFirstAsync<{ updated_at: string }>(
        'SELECT updated_at FROM goals WHERE id = ?',
        [id],
      );
      // Remote wins ties: the TUI is the primary editor.
      if (local && local.updated_at >= remoteUpdatedAt) continue;
      await db.runAsync(
        `INSERT INTO goals (id, title, description, status, parent_id, sheet_id, sort_order, created_at, updated_at, completed_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
           title = excluded.title, description = excluded.description,
           status = excluded.status, parent_id = excluded.parent_id,
           sheet_id = excluded.sheet_id, sort_order = excluded.sort_order,
           updated_at = excluded.updated_at, completed_at = excluded.completed_at`,
        [
          id,
          String(g.title ?? ''),
          g.description == null ? null : String(g.description),
          String(g.status ?? 'pending'),
          g.parent_id == null ? null : String(g.parent_id),
          g.sheet_id == null ? null : String(g.sheet_id),
          Number(g.sort_order ?? 0),
          String(g.created_at ?? ''),
          remoteUpdatedAt,
          g.completed_at == null ? null : String(g.completed_at),
        ],
      );
      pulledGoals += 1;
    }

    // --- goals: push local rows newer than the last push ---
    const lastPush = (await getMeta(LAST_PUSH_KEY)) ?? '';
    const dirty = await db.getAllAsync<Record<string, unknown>>(
      `SELECT ${GOAL_COLS} FROM goals WHERE updated_at > ?`,
      [lastPush],
    );
    if (dirty.length > 0) {
      await pipeline(
        creds,
        dirty.map((g) => ({
          sql: `INSERT INTO goals (id, title, description, status, parent_id, sheet_id, sort_order, created_at, updated_at, completed_at)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(id) DO UPDATE SET
                  title = excluded.title, description = excluded.description,
                  status = excluded.status, parent_id = excluded.parent_id,
                  sheet_id = excluded.sheet_id, sort_order = excluded.sort_order,
                  updated_at = excluded.updated_at, completed_at = excluded.completed_at`,
          args: toHranaArgs([
            String(g.id),
            String(g.title ?? ''),
            g.description == null ? '' : String(g.description),
            String(g.status ?? 'pending'),
            g.parent_id == null ? '' : String(g.parent_id),
            g.sheet_id == null ? '' : String(g.sheet_id),
            Number(g.sort_order ?? 0),
            String(g.created_at ?? ''),
            String(g.updated_at ?? ''),
            g.completed_at == null ? '' : String(g.completed_at),
          ]),
        })),
      );
      pushedGoals = dirty.length;
    }

    // --- settings: pull-only merge (remote wins), excluding local keys ---
    const settingsResult = await pipeline(creds, [{ sql: 'SELECT key, value FROM settings' }]);
    for (const row of rowsToObjects(settingsResult[0])) {
      const key = String(row.key ?? '');
      if (isLocalOnlyKey(key)) continue;
      await db.runAsync(
        'INSERT INTO settings (key, value) VALUES (?, ?) ON CONFLICT(key) DO UPDATE SET value = excluded.value',
        [key, String(row.value ?? '')],
      );
    }

    // --- themes: pull-only; derive is_dark from background luminance ---
    const themesResult = await pipeline(creds, [
      { sql: 'SELECT id, name, source, colors_json FROM themes' },
    ]);
    for (const row of rowsToObjects(themesResult[0])) {
      const id = String(row.id ?? '');
      const colorsJson = String(row.colors_json ?? '{}');
      let colors: Record<string, string> = {};
      try {
        colors = JSON.parse(colorsJson);
      } catch {
        // bad row — keep defaults
      }
      const bg = colors.background ?? colors.bg ?? '#000000';
      const hex = bg.replace('#', '');
      const r = Number.parseInt(hex.slice(0, 2), 16) || 0;
      const g = Number.parseInt(hex.slice(2, 4), 16) || 0;
      const b = Number.parseInt(hex.slice(4, 6), 16) || 0;
      const isDark = 0.299 * r + 0.587 * g + 0.114 * b < 128 ? 1 : 0;
      await db.runAsync(
        `INSERT INTO themes (id, name, source, is_dark, colors_json, last_used_at)
         VALUES (?, ?, ?, ?, ?, NULL)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name, colors_json = excluded.colors_json`,
        [id, String(row.name ?? id), String(row.source ?? 'plugin'), isDark, colorsJson],
      );
    }
  } catch (e) {
    // Every sync failure is recorded in the on-device error log so it
    // shows up on the profile page. logError never throws.
    const message = e instanceof Error ? e.message : String(e);
    await logError('sync', `sync failed: ${message}`);
    return {
      ok: false,
      message,
      pulledGoals,
      pushedGoals,
    };
  }

  const now = new Date().toISOString();
  await setMeta(LAST_PULL_KEY, now);
  await setMeta(LAST_PUSH_KEY, now);

  return {
    ok: true,
    message: `synced · ${pulledGoals} in · ${pushedGoals} out`,
    pulledGoals,
    pushedGoals,
  };
}
