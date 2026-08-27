/**
 * Local SQLite database layer for the mobile client.
 *
 * The schema + migrations are the SHARED contract, generated from the
 * canonical source in the Rust workspace:
 *
 *   rust/schema/schema.sql            (tables)
 *   rust/crates/schema MIGRATIONS     (versioned migrations)
 *
 * ...via `cargo run -p cordanui-schema --example export_ts`, which writes
 * ./schema.generated.ts (committed; a Rust drift test fails if stale).
 * Mobile runs the exact same migration list as the TUI and records
 * applied versions in the same `_migrations` table, so any database file
 * has identical schema state no matter which client created it.
 */

import * as Crypto from 'expo-crypto';
import * as SQLite from 'expo-sqlite';

import {
  LATEST_SCHEMA_VERSION,
  SCHEMA_SQL,
  SHARED_MIGRATIONS,
} from './schema.generated';
import { DARK_THEME_COLORS, LIGHT_THEME_COLORS } from '@/theme/types';
import type { CreateGoalInput, Goal, GoalSheet, UpdateGoalInput } from '@/types/goal';

const DB_NAME = 'cordanui.db';

/**
 * Memoized as a *promise*, not a value: concurrent callers (e.g. initDb and
 * the first screen refresh at startup) must all await the same open+migrate
 * sequence instead of querying a half-initialized database.
 */
let dbPromise: Promise<SQLite.SQLiteDatabase> | null = null;

export function getDb(): Promise<SQLite.SQLiteDatabase> {
  if (!dbPromise) {
    dbPromise = openAndMigrate().catch((e) => {
      // Allow a later retry instead of caching the failure forever.
      dbPromise = null;
      throw e;
    });
  }
  return dbPromise;
}

async function openAndMigrate(): Promise<SQLite.SQLiteDatabase> {
  const database = await SQLite.openDatabaseAsync(DB_NAME);
  await database.execAsync(SCHEMA_SQL);
  await migrate(database);
  await database.execAsync('PRAGMA foreign_keys = ON;');
  return database;
}

/**
 * (Re-)insert the two builtin theme rows. Idempotent (INSERT OR IGNORE).
 * Called by migration v2 AND by purgeAllData — wiping the themes table
 * must restore these, because schema_migrations still records v2 as
 * applied and that seeding migration will never run again on its own.
 */
async function seedBuiltinThemes(database: SQLite.SQLiteDatabase): Promise<void> {
  await database.runAsync(
    `INSERT OR IGNORE INTO themes (id, name, source, colors_json)
     VALUES (?, ?, 'builtin', ?)`,
    ['builtin-dark', 'Cordanui Dark', JSON.stringify(DARK_THEME_COLORS)],
  );
  await database.runAsync(
    `INSERT OR IGNORE INTO themes (id, name, source, colors_json)
     VALUES (?, ?, 'builtin', ?)`,
    ['builtin-light', 'Cordanui Light', JSON.stringify(LIGHT_THEME_COLORS)],
  );
}

/**
 * Compares the applied versions in `schema_migrations` against
 * LATEST_SCHEMA_VERSION and runs every pending step, oldest first, each in
 * its own transaction with the applied version recorded after it commits.
 */
async function migrate(database: SQLite.SQLiteDatabase): Promise<void> {
  // --- legacy alignment ---
  // Databases created before the shared-schema switch used a mobile-only
  // migration list recorded in `schema_migrations`. Bring them to the
  // shared baseline with code (schema-shape guards instead of blind DDL),
  // then adopt the shared `_migrations` bookkeeping.
  const legacy = await tableExists(database, 'schema_migrations');
  if (legacy) {
    await alignLegacyMobileDb(database);
    await database.execAsync('DROP TABLE schema_migrations');
  }

  // --- shared migrations (identical to the TUI) ---
  for (const m of SHARED_MIGRATIONS) {
    const done = await database.getFirstAsync<{ v: number | null }>(
      'SELECT 1 AS v FROM _migrations WHERE version = ?',
      [m.version],
    );
    if (done) continue;
    await database.withTransactionAsync(async () => {
      await database.execAsync(m.sql);
      await database.runAsync(
        'INSERT INTO _migrations (version, name, applied_at) VALUES (?, ?, ?)',
        [m.version, m.name, now()],
      );
    });
  }

  // Self-heal: purged databases lost their builtin theme rows while
  // _migrations still says "applied". INSERT OR IGNORE — free when present.
  await seedBuiltinThemes(database);

  await verifySchema(database);
}

/** True if the table exists in sqlite_master. */
async function tableExists(database: SQLite.SQLiteDatabase, name: string): Promise<boolean> {
  const row = await database.getFirstAsync<{ n: number }>(
    "SELECT COUNT(*) AS n FROM sqlite_master WHERE type = 'table' AND name = ?",
    [name],
  );
  return (row?.n ?? 0) > 0;
}

/**
 * One-time shape alignment for pre-shared-schema mobile databases:
 * add sheet_id if missing, drop the legacy is_dark column if present,
 * merge errors_mobile into errors, and record every shared migration as
 * applied (the shapes they produce are already in place).
 */
async function alignLegacyMobileDb(database: SQLite.SQLiteDatabase): Promise<void> {
  const goalCols = await database.getAllAsync<{ name: string }>(
    'PRAGMA table_info(goals)',
  );
  const hasSheetId = goalCols.some((c) => c.name === 'sheet_id');
  if (!hasSheetId) {
    await database.execAsync(
      'ALTER TABLE goals ADD COLUMN sheet_id TEXT REFERENCES goal_sheets(id) ON DELETE SET NULL',
    );
    await database.execAsync(
      'CREATE INDEX IF NOT EXISTS idx_goals_sheet_id ON goals(sheet_id)',
    );
  }

  const themeCols = await database.getAllAsync<{ name: string }>(
    'PRAGMA table_info(themes)',
  );
  if (themeCols.some((c) => c.name === 'is_dark')) {
    // DROP COLUMN needs SQLite >= 3.35; rebuild the table if unavailable.
    try {
      await database.execAsync('ALTER TABLE themes DROP COLUMN is_dark');
    } catch {
      // Rebuild with the canonical shape (keeps the primary key — a plain
      // CREATE AS SELECT would drop it and break ON CONFLICT upserts).
      await database.execAsync(
        'CREATE TABLE themes_new (' +
          'id TEXT PRIMARY KEY, name TEXT NOT NULL, ' +
          "source TEXT NOT NULL DEFAULT 'builtin', " +
          'colors_json TEXT NOT NULL, last_used_at TEXT)',
      );
      await database.execAsync(
        'INSERT INTO themes_new (id, name, source, colors_json, last_used_at) ' +
          'SELECT id, name, source, colors_json, last_used_at FROM themes',
      );
      await database.execAsync('DROP TABLE themes');
      await database.execAsync('ALTER TABLE themes_new RENAME TO themes');
    }
  }

  if (await tableExists(database, 'errors_mobile')) {
    await database.execAsync(
      'INSERT OR IGNORE INTO errors (id, context, message, detail, created_at) ' +
      'SELECT id, context, message, detail, created_at FROM errors_mobile',
    );
    await database.execAsync('DROP TABLE errors_mobile');
  }

  // Adopt the shared bookkeeping: every shared migration's resulting
  // shape is already present, so record them all as applied.
  for (const m of SHARED_MIGRATIONS) {
    await database.runAsync(
      'INSERT OR REPLACE INTO _migrations (version, name, applied_at) VALUES (?, ?, ?)',
      [m.version, m.name, now()],
    );
  }
}
async function verifySchema(database: SQLite.SQLiteDatabase): Promise<void> {
  const tables = await database.getAllAsync<{ name: string }>(
    "SELECT name FROM sqlite_master WHERE type = 'table'",
  );
  const tableNames = new Set(tables.map((t) => t.name));
  for (const t of ['goals', 'goal_sheets', 'errors']) {
    if (!tableNames.has(t)) {
      throw new Error(`Local DB schema invalid: missing table "${t}"`);
    }
  }

  const cols = await database.getAllAsync<{ name: string }>('PRAGMA table_info(goals)');
  const colNames = new Set(cols.map((c) => c.name));
  for (const c of [
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
    'deleted_at',
  ]) {
    if (!colNames.has(c)) {
      throw new Error(`Local DB schema invalid: goals."${c}" is missing`);
    }
  }
}

function now(): string {
  return new Date().toISOString();
}

// ---------- public API ----------

export async function initDb(): Promise<void> {
  const database = await getDb();
  // Bootstrap: make sure at least one sheet exists and orphaned goals
  // (from before sheets existed) land in it.
  let first = await getFirstSheet();
  if (!first) {
    first = await createSheet('General');
  }
  await database.runAsync('UPDATE goals SET sheet_id = ? WHERE sheet_id IS NULL', [first.id]);
}

// ---------- sheets ----------

async function getFirstSheet(): Promise<GoalSheet | null> {
  const database = await getDb();
  const row = await database.getFirstAsync<GoalSheet>(
    'SELECT * FROM goal_sheets WHERE deleted_at IS NULL ORDER BY sort_order, created_at LIMIT 1',
  );
  return row ?? null;
}

export async function getSheets(): Promise<GoalSheet[]> {
  const database = await getDb();
  return database.getAllAsync<GoalSheet>(
    'SELECT * FROM goal_sheets WHERE deleted_at IS NULL ORDER BY sort_order, created_at',
  );
}

export async function createSheet(name: string): Promise<GoalSheet> {
  const database = await getDb();
  const id = Crypto.randomUUID();
  const ts = now();
  const maxRow = await database.getFirstAsync<{ m: number | null }>(
    'SELECT MAX(sort_order) AS m FROM goal_sheets',
  );
  await database.runAsync(
    'INSERT INTO goal_sheets (id, name, sort_order, created_at) VALUES (?, ?, ?, ?)',
    [id, name, (maxRow?.m ?? -1) + 1, ts],
  );
  return { id, name, sort_order: maxRow?.m != null ? maxRow.m + 1 : 0, created_at: ts };
}

export async function renameSheet(id: string, name: string): Promise<void> {
  const database = await getDb();
  await database.runAsync('UPDATE goal_sheets SET name = ? WHERE id = ?', [name, id]);
}

export async function deleteSheet(id: string): Promise<void> {
  const database = await getDb();
  // Soft-delete (tombstone) so the deletion propagates via sync. The goals
  // in this sheet are left in place (their sheet_id FK ON DELETE SET NULL
  // no longer fires — they keep referencing the tombstoned sheet id, which
  // reads treat as "no sheet" since the sheet is filtered out).
  await database.runAsync(
    'UPDATE goal_sheets SET deleted_at = ? WHERE id = ? AND deleted_at IS NULL',
    [now(), id],
  );
}

// ---------- danger zone ----------

/**
 * Purge ALL user data: goals, sheets, themes, plugin/theme settings, and
 * the on-device error log. Device-local sync credentials (`turso_url`,
 * `turso_token`) and sync bookkeeping (`sync.*`) are deliberately kept so
 * sync stays configured after a purge — mirrors the TUI's purge behavior.
 *
 * Purge is a local-only hard delete (danger zone). It does NOT tombstone,
 * so rows already pushed to the cloud will reappear on the next sync
 * pull — purge is meant for a fresh start on this device, not a global
 * wipe. Use individual delete (tombstone) for deletions that propagate.
 */
export async function purgeAllData(): Promise<void> {
  const db = await getDb();
  // withTransactionAsync (NOT manual BEGIN/COMMIT): awaited statements in
  // between let unrelated queries interleave into the open transaction,
  // which wedges the connection and surfaces as native-call rejections.
  await db.withTransactionAsync(async () => {
    await db.runAsync('DELETE FROM goals');
    await db.runAsync('DELETE FROM goal_sheets');
    await db.runAsync('DELETE FROM themes');
    await db.runAsync(
      "DELETE FROM settings WHERE key NOT IN ('turso_url', 'turso_token') AND key NOT LIKE 'sync.%'",
    );
    // Reset the incremental-sync cursors. Keeping them would make the next
    // pull skip every remote row older than the cursor — i.e. after a
    // purge, sync would appear to do nothing while the cloud still has
    // data. Cursors reset = next sync pulls the full remote state.
    await db.runAsync("DELETE FROM settings WHERE key LIKE 'sync.%'");
    await db.runAsync('DELETE FROM errors');
    await db.runAsync('DELETE FROM _outbox');
    await db.runAsync('DELETE FROM _sync_state');
    // Restore the builtin theme rows: schema_migrations still records
    // v2/v3 as applied, so their seeding migrations will never re-run.
    await seedBuiltinThemes(db);
  });
}

// ---------- goals ----------

export async function getAllGoals(sheetId?: string): Promise<Goal[]> {
  const database = await getDb();
  if (sheetId) {
    return database.getAllAsync<Goal>(
      `SELECT * FROM goals WHERE sheet_id = ? AND deleted_at IS NULL
       ORDER BY parent_id IS NOT NULL, parent_id, sort_order, created_at`,
      [sheetId],
    );
  }
  return database.getAllAsync<Goal>(
    'SELECT * FROM goals WHERE deleted_at IS NULL ORDER BY parent_id IS NOT NULL, parent_id, sort_order, created_at',
  );
}

export async function getGoal(id: string): Promise<Goal | null> {
  const database = await getDb();
  const row = await database.getFirstAsync<Goal>(
    'SELECT * FROM goals WHERE id = ? AND deleted_at IS NULL',
    [id],
  );
  return row ?? null;
}

export async function createGoal(input: CreateGoalInput): Promise<Goal> {
  const database = await getDb();
  const id = Crypto.randomUUID();
  const ts = now();
  // Append to the bottom of its sibling list unless explicitly positioned.
  const nextRow = await database.getFirstAsync<{ next: number }>(
    `SELECT COALESCE(MAX(sort_order), -1) + 1 AS next FROM goals
     WHERE parent_id IS ? AND sheet_id IS ? AND deleted_at IS NULL`,
    [input.parent_id ?? null, input.sheet_id ?? null],
  );
  const sortOrder = input.sort_order ?? nextRow?.next ?? 0;
  await database.runAsync(
    `INSERT INTO goals
       (id, title, description, status, parent_id, sheet_id, sort_order, created_at, updated_at)
     VALUES (?, ?, ?, 'pending', ?, ?, ?, ?, ?)`,
    [
      id,
      input.title,
      input.description ?? null,
      input.parent_id ?? null,
      input.sheet_id ?? null,
      sortOrder,
      ts,
      ts,
    ],
  );
  const created = await getGoal(id);
  if (!created) throw new Error('createGoal: insert returned no row');
  return created;
}

export async function updateGoal(id: string, input: UpdateGoalInput): Promise<Goal | null> {
  const database = await getDb();
  const fields: string[] = [];
  const values: (string | number | null)[] = [];

  const setIf = (field: string, value: string | number | null | undefined) => {
    if (value !== undefined) {
      fields.push(`${field} = ?`);
      values.push(value);
    }
  };

  setIf('title', input.title);
  setIf('description', input.description);
  setIf('status', input.status);
  setIf('sort_order', input.sort_order);
  setIf('completed_at', input.completed_at);
  setIf('agent_status', input.agent_status);
  setIf('agent_result', input.agent_result);
  setIf('agent_progress', input.agent_progress);
  setIf('metadata', input.metadata);

  if (fields.length === 0) return getGoal(id);

  fields.push('updated_at = ?');
  values.push(now());
  values.push(id);

  await database.runAsync(`UPDATE goals SET ${fields.join(', ')} WHERE id = ?`, values);
  return getGoal(id);
}

export async function completeGoal(id: string): Promise<Goal | null> {
  const ts = now();
  return updateGoal(id, { status: 'completed', completed_at: ts });
}

export async function uncompleteGoal(id: string): Promise<Goal | null> {
  return updateGoal(id, { status: 'pending', completed_at: null });
}

export async function deleteGoal(id: string): Promise<void> {
  const database = await getDb();
  const ts = now();
  // Soft-delete (tombstone) this goal and all descendants so the deletion
  // propagates to other clients via sync. We set deleted_at + bump
  // updated_at (wins LWW); reads filter deleted_at IS NULL.
  const ids = await collectSubtree(database, id);
  if (ids.length === 0) return;
  const placeholders = ids.map(() => '?').join(', ');
  await database.runAsync(
    `UPDATE goals SET deleted_at = ?, updated_at = ? WHERE id IN (${placeholders})`,
    [ts, ts, ...ids],
  );
}

/** Collect a goal's ID and every descendant ID (any depth). */
async function collectSubtree(database: SQLite.SQLiteDatabase, root: string): Promise<string[]> {
  const out: string[] = [];
  const stack: string[] = [root];
  while (stack.length > 0) {
    const id = stack.pop()!;
    out.push(id);
    const children = await database.getAllAsync<{ id: string }>(
      'SELECT id FROM goals WHERE parent_id = ? AND deleted_at IS NULL',
      [id],
    );
    for (const c of children) stack.push(c.id);
  }
  return out;
}

/** Persist a reorder: explicit sort_order values for the affected siblings. */
export async function updateSortOrders(
  orders: { id: string; sort_order: number }[],
): Promise<void> {
  const database = await getDb();
  await database.withTransactionAsync(async () => {
    for (const o of orders) {
      await database.runAsync('UPDATE goals SET sort_order = ?, updated_at = ? WHERE id = ?', [
        o.sort_order,
        now(),
        o.id,
      ]);
    }
  });
}

export async function getChildren(parentId: string): Promise<Goal[]> {
  const database = await getDb();
  return database.getAllAsync<Goal>(
    'SELECT * FROM goals WHERE parent_id = ? AND deleted_at IS NULL ORDER BY sort_order, created_at',
    [parentId],
  );
}

export async function getRoots(): Promise<Goal[]> {
  const database = await getDb();
  return database.getAllAsync<Goal>(
    'SELECT * FROM goals WHERE parent_id IS NULL AND deleted_at IS NULL ORDER BY sort_order, created_at',
  );
}
