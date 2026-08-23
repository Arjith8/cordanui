/**
 * Local SQLite database layer for the mobile client.
 *
 * Phase 1 (local-first): backed by expo-sqlite. When phase 2 (Turso sync)
 * lands, this module's internals swap to libSQL — the public API stays the
 * same, only the driver changes.
 *
 * The schema is the shared contract (see rust/schema/schema.sql). This file
 * embeds the same schema so the mobile app can bootstrap a local DB on
 * first run.
 */

import * as Crypto from 'expo-crypto';
import * as SQLite from 'expo-sqlite';

import { DARK_THEME_COLORS, LIGHT_THEME_COLORS } from '@/theme/types';
import type { CreateGoalInput, Goal, GoalSheet, UpdateGoalInput } from '@/types/goal';

const DB_NAME = 'cordanui.db';

const SCHEMA_SQL = `
CREATE TABLE IF NOT EXISTS schema_migrations (
    version    INTEGER PRIMARY KEY,
    name       TEXT NOT NULL,
    applied_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS goals (
    id           TEXT PRIMARY KEY,
    title        TEXT NOT NULL,
    description  TEXT,
    status       TEXT NOT NULL DEFAULT 'pending',
    parent_id    TEXT REFERENCES goals(id) ON DELETE CASCADE,
    sort_order   INTEGER NOT NULL DEFAULT 0,
    created_at   TEXT NOT NULL,
    updated_at   TEXT NOT NULL,
    completed_at TEXT,
    agent_status   TEXT,
    agent_result   TEXT,
    agent_progress TEXT,
    metadata      TEXT
);

CREATE TABLE IF NOT EXISTS goal_sheets (
    id         TEXT PRIMARY KEY,
    name       TEXT NOT NULL,
    sort_order INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS errors_mobile (
    id         TEXT PRIMARY KEY,
    context    TEXT NOT NULL,
    message    TEXT NOT NULL,
    detail     TEXT,
    created_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS themes (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    source       TEXT NOT NULL DEFAULT 'builtin',
    is_dark      INTEGER NOT NULL DEFAULT 0,
    colors_json  TEXT NOT NULL,
    last_used_at TEXT
);

CREATE TABLE IF NOT EXISTS settings (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_goals_parent_id ON goals(parent_id);
CREATE INDEX IF NOT EXISTS idx_goals_status ON goals(status);
CREATE INDEX IF NOT EXISTS idx_goals_sort_order ON goals(sort_order);
CREATE INDEX IF NOT EXISTS idx_errors_created_at ON errors_mobile(created_at);
`;

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
 * The newest schema version this build of the app understands. Bump this and
 * add a matching entry to MIGRATIONS whenever the schema changes. On open we
 * compare the version recorded in the local `schema_migrations` table against
 * this variable and apply everything missing.
 */
export const LATEST_SCHEMA_VERSION = 2;

interface Migration {
  version: number;
  name: string;
  up: (database: SQLite.SQLiteDatabase) => Promise<void>;
}

const MIGRATIONS: Migration[] = [
  {
    version: 1,
    name: 'goal-sheets',
    // Adds sheet tracking. Guarded so it is a no-op on databases that already
    // received this change (fresh installs included).
    up: async (database) => {
      const cols = await database.getAllAsync<{ name: string }>('PRAGMA table_info(goals)');
      const hasSheetId = cols.some((c) => c.name === 'sheet_id');
      if (!hasSheetId) {
        await database.execAsync(
          'ALTER TABLE goals ADD COLUMN sheet_id TEXT REFERENCES goal_sheets(id) ON DELETE CASCADE',
        );
        await database.execAsync(
          'CREATE INDEX IF NOT EXISTS idx_goals_sheet_id ON goals(sheet_id)',
        );
      }
    },
  },
  {
    version: 2,
    name: 'theme-system',
    // Themes (builtin now, plugin-provided later) + a settings KV store.
    // Tables are created by SCHEMA_SQL on fresh installs; this seeds the
    // builtin themes and is guarded for every other case.
    up: async (database) => {
      await database.runAsync(
        `INSERT OR IGNORE INTO themes (id, name, source, is_dark, colors_json)
         VALUES (?, ?, 'builtin', 1, ?)`,
        ['builtin-dark', 'Cordanui Dark', JSON.stringify(DARK_THEME_COLORS)],
      );
      await database.runAsync(
        `INSERT OR IGNORE INTO themes (id, name, source, is_dark, colors_json)
         VALUES (?, ?, 'builtin', 0, ?)`,
        ['builtin-light', 'Cordanui Light', JSON.stringify(LIGHT_THEME_COLORS)],
      );
    },
  },
];

/**
 * Compares the applied versions in `schema_migrations` against
 * LATEST_SCHEMA_VERSION and runs every pending step, oldest first, each in
 * its own transaction with the applied version recorded after it commits.
 */
async function migrate(database: SQLite.SQLiteDatabase): Promise<void> {
  const row = await database.getFirstAsync<{ applied: number | null }>(
    'SELECT MAX(version) AS applied FROM schema_migrations',
  );
  const current = row?.applied ?? 0;

  if (current > LATEST_SCHEMA_VERSION) {
    throw new Error(
      `Database is newer (v${current}) than this app supports (v${LATEST_SCHEMA_VERSION}). Update the app.`,
    );
  }

  for (const migration of MIGRATIONS) {
    if (migration.version <= current) continue;
    await database.withTransactionAsync(() => migration.up(database));
    await database.runAsync(
      'INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)',
      [migration.version, migration.name, now()],
    );
  }

  await verifySchema(database);
}

/** Fail fast with a readable message instead of "no such column" deep in UI code. */
async function verifySchema(database: SQLite.SQLiteDatabase): Promise<void> {
  const tables = await database.getAllAsync<{ name: string }>(
    "SELECT name FROM sqlite_master WHERE type = 'table'",
  );
  const tableNames = new Set(tables.map((t) => t.name));
  for (const t of ['goals', 'goal_sheets', 'errors_mobile']) {
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
    'SELECT * FROM goal_sheets ORDER BY sort_order, created_at LIMIT 1',
  );
  return row ?? null;
}

export async function getSheets(): Promise<GoalSheet[]> {
  const database = await getDb();
  return database.getAllAsync<GoalSheet>(
    'SELECT * FROM goal_sheets ORDER BY sort_order, created_at',
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
  // ON DELETE CASCADE removes the sheet's goals.
  await database.runAsync('DELETE FROM goal_sheets WHERE id = ?', [id]);
}

// ---------- goals ----------

export async function getAllGoals(sheetId?: string): Promise<Goal[]> {
  const database = await getDb();
  if (sheetId) {
    return database.getAllAsync<Goal>(
      `SELECT * FROM goals WHERE sheet_id = ?
       ORDER BY parent_id IS NOT NULL, parent_id, sort_order, created_at`,
      [sheetId],
    );
  }
  return database.getAllAsync<Goal>(
    'SELECT * FROM goals ORDER BY parent_id IS NOT NULL, parent_id, sort_order, created_at',
  );
}

export async function getGoal(id: string): Promise<Goal | null> {
  const database = await getDb();
  const row = await database.getFirstAsync<Goal>('SELECT * FROM goals WHERE id = ?', [id]);
  return row ?? null;
}

export async function createGoal(input: CreateGoalInput): Promise<Goal> {
  const database = await getDb();
  const id = Crypto.randomUUID();
  const ts = now();
  // Append to the bottom of its sibling list unless explicitly positioned.
  const nextRow = await database.getFirstAsync<{ next: number }>(
    `SELECT COALESCE(MAX(sort_order), -1) + 1 AS next FROM goals
     WHERE parent_id IS ? AND sheet_id IS ?`,
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
  // ON DELETE CASCADE handles subgoals.
  await database.runAsync('DELETE FROM goals WHERE id = ?', [id]);
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
    'SELECT * FROM goals WHERE parent_id = ? ORDER BY sort_order, created_at',
    [parentId],
  );
}

export async function getRoots(): Promise<Goal[]> {
  const database = await getDb();
  return database.getAllAsync<Goal>(
    'SELECT * FROM goals WHERE parent_id IS NULL ORDER BY sort_order, created_at',
  );
}
