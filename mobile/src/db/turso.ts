/**
 * Turso sync — pulls and pushes shared data over the Turso HTTP pipeline
 * API (Hrana over HTTP), no extra dependencies.
 *
 * Model (mirrors the TUI's last-write-wins contract):
 * - **goals**: pulled fully and merged by `updated_at` (newer wins), pushed
 *   incrementally (rows newer than the last push). Deletes propagate via
 *   `deleted_at` soft-delete tombstones — the `deleted_at` column is part
 *   of the synced column set, and a tombstoned row's bumped `updated_at`
 *   wins LWW.
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
  return (
    key === CREDS_URL_KEY ||
    key === CREDS_TOKEN_KEY ||
    key.startsWith('sync.') ||
    key.startsWith('_')
  );
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

/** Translate Turso URL schemes to real HTTP(S) for fetch().
 *
 * Users enter `libsql://` or `turso://` URLs (as the placeholders and
 * docs suggest), but neither is an actual transport protocol: Android's
 * OkHttp throws MalformedURLException ("unknown protocol: libsql") if
 * they reach fetch() untranslated. Both schemes mean HTTPS.
 */
function httpBase(url: string): string {
  return url
    .trim()
    .replace(/^libsql:\/\//i, 'https://')
    .replace(/^turso:\/\//i, 'https://')
    .replace(/\/+$/, '');
}

/** Execute a batch of statements against the Turso HTTP pipeline API. */
async function pipeline(
  creds: TursoCreds,
  stmts: { sql: string; args?: HranaArg[] }[],
): Promise<HranaResult[]> {
  const endpoint = httpBase(creds.url) + '/v2/pipeline';
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
    const bodyText = await response.text().catch(() => '');
    throw new Error(`turso http ${response.status}: ${bodyText.slice(0, 200)}`);
  }
  const json = (await response.json()) as {
    results: (
      | { type: 'ok'; response: { results: HranaResult[] } }
      | { type: 'error'; error?: unknown }
    )[];
  };
  const out: HranaResult[] = [];
  // json.results has one entry per request INCLUDING the trailing 'close';
  // only the first stmts.length entries carry statement results.
  for (let i = 0; i < stmts.length; i++) {
    const result = json.results?.[i];
    if (!result) {
      throw new Error(
        `turso: no response for stmt #${i} (got ${json.results?.length ?? 0} results)`,
      );
    }
    if (result.type !== 'ok') {
      // Name the offending statement — stmt[i] corresponds to requests[i].
      const sql = stmts[i]?.sql ?? '?';
      throw new Error(
        `turso stmt #${i} failed (${sql.slice(0, 80)}): ${JSON.stringify(result).slice(0, 200)}`,
      );
    }
    // Hrana v2 wraps each execute response in `result` (singular object);
    // tolerate `results` (array) too for older servers.
    const inner = result.response as {
      results?: HranaResult[];
      result?: HranaResult;
    };
    const list = inner.results ?? (inner.result ? [inner.result] : []);
    if (list.length === 0) {
      throw new Error(
        `turso stmt #${i} returned no results: ${JSON.stringify(result.response).slice(0, 120)}`,
      );
    }
    out.push(...list);
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

// Columns mobile wants from a goals table, in preference order. The
// remote database may lack some of them (e.g. `sheet_id` is a
// mobile-local extension absent from the TUI/cloud schema) — syncNow
// discovers the remote's real columns each run and intersects.
const DESIRED_GOAL_COLS = [
  'id',
  'title',
  'description',
  'status',
  'parent_id',
  'sheet_id',
  'sort_order',
  'created_at',
  'updated_at',
  'completed_at',
  // Agent fields — synced so mobile sees agent status/result written by
  // the backend, and the backend sees agent_status='queued' written by
  // mobile.
  'agent_status',
  'agent_result',
  'agent_progress',
  'metadata',
  'deleted_at',
];

/** Coerce one goals cell into a Hrana arg, per column semantics. */
function goalArg(col: string, value: unknown): HranaArg {
  switch (col) {
    case 'sort_order':
      return { type: 'integer', value: Number(value ?? 0) };
    // NOT NULL text columns
    case 'id':
    case 'title':
    case 'status':
    case 'created_at':
    case 'updated_at':
      return { type: 'text', value: String(value ?? '') };
    // Nullable text columns
    default:
      return value == null || value === ''
        ? { type: 'text', value: '' }
        : { type: 'text', value: String(value) };
  }
}

/** Read the remote goals table's real column list from its DDL. */
async function discoverGoalColumns(creds: TursoCreds): Promise<string[] | null> {
  // Route 1: the table's CREATE statement from sqlite_master.
  try {
    const res = await pipeline(creds, [
      { sql: "SELECT COALESCE(sql, '') FROM sqlite_master WHERE type = 'table' AND name = 'goals'" },
    ]);
    const ddl = String((rowsToObjects(res[0])[0] ?? {}).sql ?? '');
    const cols = parseDdlColumns(ddl);
    if (cols) return cols;
  } catch {
    // Some hosts restrict sqlite_master reads — fall through.
  }

  // Route 2: PRAGMA table_info.
  try {
    const res = await pipeline(creds, [{ sql: 'PRAGMA table_info(goals)' }]);
    const rows = rowsToObjects(res[0]);
    if (rows.length > 0) {
      const cols = rows
        .map((r) => String(r.name ?? ''))
        .filter((n) => n && n !== 'cid');
      if (cols.length > 0) return cols;
    }
  } catch {
    // Fall through to the caller's error-driven retry.
  }

  return null;
}

/** Extract column names from a CREATE TABLE statement, or null. */
function parseDdlColumns(ddl: string): string[] | null {
  const open = ddl.indexOf('(');
  const close = ddl.lastIndexOf(')');
  if (open < 0 || close <= open) return null;
  const body = ddl.slice(open + 1, close);
  const constraint = /^(PRIMARY|UNIQUE|CHECK|FOREIGN|CONSTRAINT)$/i;
  const cols = body
    .split(',')
    .map((part) => part.trim().split(/[\s(]/)[0]?.replace(/"/g, '') ?? '')
    .filter((name) => name && !constraint.test(name));
  return cols.length > 0 ? cols : null;
}

/** Remote ALTER statements that safely add a missing nullable column. */
const MISSING_COL_ALTERS: Record<string, string> = {
  sheet_id:
    'ALTER TABLE goals ADD COLUMN sheet_id TEXT REFERENCES goal_sheets(id) ON DELETE SET NULL',
  deleted_at: 'ALTER TABLE goals ADD COLUMN deleted_at TEXT',
  agent_status: 'ALTER TABLE goals ADD COLUMN agent_status TEXT',
  agent_result: 'ALTER TABLE goals ADD COLUMN agent_result TEXT',
  agent_progress: 'ALTER TABLE goals ADD COLUMN agent_progress TEXT',
  metadata: 'ALTER TABLE goals ADD COLUMN metadata TEXT',
};

/** Upsert statement for an explicit column list. */function goalUpsertSql(cols: string[]): string {
  const updates = cols
    .filter((c) => c !== 'id')
    .map((c) => `${c} = excluded.${c}`)
    .join(', ');
  return `INSERT INTO goals (${cols.join(', ')}) VALUES (${cols.map(() => '?').join(', ')})
          ON CONFLICT(id) DO UPDATE SET ${updates}`;
}

/** Local (expo-sqlite) bind value for one goals cell. */
function goalLocalValue(col: string, value: unknown): string | number | null {
  switch (col) {
    case 'sort_order':
      return Number(value ?? 0);
    case 'id':
    case 'title':
    case 'status':
    case 'created_at':
    case 'updated_at':
      return String(value ?? '');
    default:
      return value == null ? null : String(value);
  }
}

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

    // --- discover which goal columns the remote actually has ---
    let remoteCols = await discoverGoalColumns(creds);
    if (remoteCols === null) {
      // Last resort: assume the full desired set and let the server tell
      // us what's missing. SQLite names the offending column in
      // "no such column: X" — strip and retry until the statement runs.
      let candidate = [...DESIRED_GOAL_COLS];
      for (let attempt = 0; attempt < DESIRED_GOAL_COLS.length; attempt++) {
        try {
          await pipeline(creds, [
            { sql: `SELECT ${candidate.join(', ')} FROM goals LIMIT 1` },
          ]);
          remoteCols = candidate;
          break;
        } catch (e) {
          const msg = e instanceof Error ? e.message : String(e);
          const match = msg.match(/no such column:\s*(\w+)/i);
          if (!match) {
            throw new Error(
              `could not determine remote goals schema: ${msg}`,
            );
          }
          candidate = candidate.filter((c) => c !== match[1]);
        }
      }
      if (remoteCols === null) {
        throw new Error('could not determine remote goals schema: every column rejected');
      }
    }
    const goalCols = DESIRED_GOAL_COLS.filter((c) => remoteCols.includes(c));
    if (!goalCols.includes('id')) {
      throw new Error('remote goals table has no id column');
    }
    const dropped = DESIRED_GOAL_COLS.filter((c) => !goalCols.includes(c));
    if (dropped.length > 0) {
      // The cloud schema lags the canonical one (e.g. the TUI hasn't run
      // the promoting migration yet). DDL works over the pipeline API, so
      // repair the remote instead of degrading the sync permanently.
      const repairs: string[] = [];
      for (const col of dropped) {
        const alter = MISSING_COL_ALTERS[col];
        if (!alter) continue;
        try {
          await pipeline(creds, [
            {
              sql: 'CREATE TABLE IF NOT EXISTS goal_sheets (id TEXT PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL)',
            },
            { sql: alter },
          ]);
          repairs.push(col);
        } catch (e) {
          await logError(
            'sync',
            `could not add missing column '${col}' to remote`,
            e instanceof Error ? e.message : String(e),
          );
        }
      }
      if (repairs.length > 0) {
        // Re-discover with the repaired schema.
        const rediscovered = await discoverGoalColumns(creds);
        if (rediscovered) {
          goalCols.length = 0;
          goalCols.push(...DESIRED_GOAL_COLS.filter((c) => rediscovered.includes(c)));
        }
        await logError(
          'sync',
          `remote schema repaired, added columns: ${repairs.join(', ')}`,
          undefined,
        );
      }
      const still = DESIRED_GOAL_COLS.filter((c) => !goalCols.includes(c));
      if (still.length > 0) {
        await logError(
          'sync',
          `remote goals table lacks columns: ${still.join(', ')}`,
          'mobile will sync the shared columns only; consider aligning schemas',
        );
      }
    }

    // --- goals: pull everything and merge LWW by updated_at ---
    const lastPull = (await getMeta(LAST_PULL_KEY)) ?? '';
    const results = await pipeline(creds, [
      {
        sql: `SELECT ${goalCols.join(', ')} FROM goals WHERE updated_at > ?`,
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
        goalUpsertSql(goalCols),
        goalCols.map((c) => goalLocalValue(c, g[c])),
      );
      pulledGoals += 1;
    }

    // --- goals: push local rows newer than the last push ---
    const lastPush = (await getMeta(LAST_PUSH_KEY)) ?? '';
    const dirty = await db.getAllAsync<Record<string, unknown>>(
      `SELECT ${goalCols.join(', ')} FROM goals WHERE updated_at > ?`,
      [lastPush],
    );
    if (dirty.length > 0) {
      await pipeline(
        creds,
        dirty.map((g) => ({
          sql: goalUpsertSql(goalCols),
          args: goalCols.map((c) => goalArg(c, g[c])),
        })),
      );
      pushedGoals = dirty.length;
    }

    // --- goal_sheets: pull remote sheets (small table, full replace) ---
    try {
      const sheetsResult = await pipeline(creds, [
        { sql: 'SELECT id, name, created_at, deleted_at FROM goal_sheets' },
      ]);
      for (const row of rowsToObjects(sheetsResult[0])) {
        const id = String(row.id ?? '');
        await db.runAsync(
          `INSERT INTO goal_sheets (id, name, created_at, deleted_at)
           VALUES (?, ?, ?, ?)
           ON CONFLICT(id) DO UPDATE SET
             name = excluded.name, deleted_at = excluded.deleted_at`,
          [
            id,
            String(row.name ?? id),
            String(row.created_at ?? new Date().toISOString()),
            row.deleted_at == null ? null : String(row.deleted_at),
          ],
        );
      }
      // Push local sheets.
      const localSheets = await db.getAllAsync<Record<string, unknown>>(
        'SELECT id, name, created_at, deleted_at FROM goal_sheets',
      );
      if (localSheets.length > 0) {
        await pipeline(
          creds,
          localSheets.map((s) => ({
            sql: `INSERT INTO goal_sheets (id, name, created_at, deleted_at)
                  VALUES (?, ?, ?, ?)
                  ON CONFLICT(id) DO UPDATE SET
                    name = excluded.name, deleted_at = excluded.deleted_at`,
            args: [
              { type: 'text' as const, value: String(s.id) },
              { type: 'text' as const, value: String(s.name ?? '') },
              {
                type: 'text' as const,
                value: String(s.created_at ?? new Date().toISOString()),
              },
              s.deleted_at == null
                ? { type: 'text' as const, value: '' }
                : { type: 'text' as const, value: String(s.deleted_at) },
            ],
          })),
        );
      }
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (msg.includes('no such table') || msg.includes('no such column')) {
        await logError('sync', `sheets sync skipped: ${msg}`);
      } else {
        throw e;
      }
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

    // --- themes: pull-only (TUI is the theme writer) ---
    let themesResult: Awaited<ReturnType<typeof pipeline>>;
    try {
      themesResult = await pipeline(creds, [
        { sql: 'SELECT id, name, source, colors_json, last_used_at, deleted_at FROM themes' },
      ]);
    } catch (e) {
      const msg = e instanceof Error ? e.message : String(e);
      if (msg.includes('no such column') || msg.includes('no such table')) {
        // Remote hasn't migrated yet — fall back to old column set.
        themesResult = await pipeline(creds, [
          { sql: 'SELECT id, name, source, colors_json FROM themes' },
        ]);
      } else {
        throw e;
      }
    }
    for (const row of rowsToObjects(themesResult[0])) {
      const id = String(row.id ?? '');
      const colorsJson = String(row.colors_json ?? '{}');
      await db.runAsync(
        `INSERT INTO themes (id, name, source, colors_json, last_used_at, deleted_at)
         VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET
           name = excluded.name, colors_json = excluded.colors_json,
           last_used_at = excluded.last_used_at, deleted_at = excluded.deleted_at`,
        [
          id,
          String(row.name ?? id),
          String(row.source ?? 'plugin'),
          colorsJson,
          row.last_used_at == null ? null : String(row.last_used_at),
          row.deleted_at == null ? null : String(row.deleted_at),
        ],
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
