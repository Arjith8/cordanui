/**
 * Error tracking on-device. Every caught error in the app is logged into the
 * errors_mobile table so it can be reviewed on the profile/errors page.
 *
 * logError never throws: error logging must not be able to cause errors.
 */

import * as Crypto from 'expo-crypto';

import { getDb } from '@/db/goalsDb';

export interface LoggedError {
  id: string;
  context: string;
  message: string;
  detail: string | null;
  created_at: string;
}

export async function logError(context: string, error: unknown, detail?: string): Promise<void> {
  try {
    const db = await getDb();
    const message = error instanceof Error ? error.message : String(error);
    const stack = error instanceof Error ? (error.stack ?? null) : null;
    await db.runAsync(
      'INSERT INTO errors_mobile (id, context, message, detail, created_at) VALUES (?, ?, ?, ?, ?)',
      [Crypto.randomUUID(), context, message, detail ?? stack, new Date().toISOString()],
    );
  } catch {
    // Swallow — logging must not break the app.
  }
}

export async function getErrors(limit = 200): Promise<LoggedError[]> {
  const db = await getDb();
  return db.getAllAsync<LoggedError>(
    'SELECT * FROM errors_mobile ORDER BY created_at DESC LIMIT ?',
    [limit],
  );
}

export async function clearErrors(): Promise<void> {
  const db = await getDb();
  await db.runAsync('DELETE FROM errors_mobile');
}
