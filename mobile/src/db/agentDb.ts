/**
 * Agent backend integration for mobile.
 *
 * The agent backend is an optional component that runs provider plugins
 * against queued goals. Mobile discovers it through the synced
 * `settings` table: the TUI writes `agent.url` when it has an active
 * provider plugin, and mobile reads it to show/hide agent triggers.
 *
 * Flow:
 * 1. Mobile reads `agent.url` from the local settings table (synced
 *    from Turso, written by the TUI).
 * 2. User long-presses a goal → "Assign to agent".
 * 3. Mobile writes `status = 'agent_mode'`, `agent_status = 'queued'`
 *    to the local DB → syncs to Turso.
 * 4. Mobile POSTs `{ task_id }` to `{agentUrl}/wake` — a wake-and-point,
 *    not a data transfer.
 * 5. The backend reads the task from Turso, runs the provider, writes
 *    results back to Turso.
 * 6. Mobile sees the result via sync and renders it.
 */

import { getDb } from '@/db/goalsDb';
import { getConfig } from '@/config';
import type { Goal } from '@/types/goal';

/** Setting key written by the TUI to announce agent capability. */
const AGENT_URL_KEY = 'agent.url';

/**
 * Get the agent backend URL. Priority:
 * 1. Synced `agent.url` setting (written by the TUI) — non-empty means
 *    the TUI has an active provider plugin.
 * 2. `EXPO_PUBLIC_AGENT_URL` env var (fallback for direct backend users).
 * Returns null if neither is set, meaning agent triggers are hidden.
 */
export async function getAgentUrl(): Promise<string | null> {
  // 1. Check the synced settings table.
  const db = await getDb();
  const row = await db.getFirstAsync<{ value: string }>(
    'SELECT value FROM settings WHERE key = ?',
    [AGENT_URL_KEY],
  );
  if (row?.value && row.value.trim() !== '') {
    return row.value.trim();
  }

  // 2. Fall back to the env var.
  const envUrl = getConfig().agentUrl;
  if (envUrl && envUrl.trim() !== '') {
    return envUrl.trim();
  }

  return null;
}

/**
 * Whether agent triggers should be visible. True when a non-empty
 * `agent.url` is available (from synced settings or env var).
 */
export async function isAgentAvailable(): Promise<boolean> {
  return (await getAgentUrl()) !== null;
}

/**
 * Assign a goal to the agent backend.
 *
 * Writes `status = 'agent_mode'`, `agent_status = 'queued'` to the local
 * DB (syncs to Turso), then POSTs a wake-and-point to the backend. The
 * backend reads the task from Turso and runs the provider plugin.
 *
 * Returns true if the wake call succeeded, false otherwise (the goal is
 * still queued locally — the backend's poll loop will pick it up).
 */
export async function assignToAgent(goalId: string): Promise<boolean> {
  const db = await getDb();
  const now = new Date().toISOString();

  // Write agent_mode + queued to the local DB.
  await db.runAsync(
    `UPDATE goals
     SET status = 'agent_mode', agent_status = 'queued',
         agent_result = NULL, agent_progress = NULL, updated_at = ?
     WHERE id = ?`,
    [now, goalId],
  );

  // Try to wake the backend. A failed wake is not fatal — the poll loop
  // picks up queued tasks on its own. We just want it to start sooner.
  const agentUrl = await getAgentUrl();
  if (!agentUrl) {
    return false;
  }

  try {
    const response = await fetch(`${agentUrl.replace(/\/+$/, '')}/wake`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ task_id: goalId }),
    });
    return response.ok;
  } catch {
    // Network error — the goal is still queued locally and will sync to
    // Turso, where the backend's poll loop will find it.
    return false;
  }
}

/**
 * Unassign a goal from the agent — revert to 'pending' and clear agent
 * fields. Useful if the user changes their mind before the backend picks
 * it up.
 */
export async function unassignFromAgent(goalId: string): Promise<void> {
  const db = await getDb();
  const now = new Date().toISOString();
  await db.runAsync(
    `UPDATE goals
     SET status = 'pending', agent_status = NULL,
         agent_result = NULL, agent_progress = NULL, updated_at = ?
     WHERE id = ?`,
    [now, goalId],
  );
}

/**
 * Parse the `agent_result` JSON string on a goal into a structured shape.
 * Returns null if the result is absent or not valid JSON.
 */
export interface AgentResult {
  content: string;
  files?: Array<{ path: string; content?: string | null }>;
}

export function parseAgentResult(goal: Goal): AgentResult | null {
  if (!goal.agent_result) return null;
  try {
    const parsed = JSON.parse(goal.agent_result);
    if (typeof parsed === 'string') {
      return { content: parsed };
    }
    if (parsed && typeof parsed.content === 'string') {
      return parsed as AgentResult;
    }
    return { content: JSON.stringify(parsed, null, 2) };
  } catch {
    // Not JSON — treat the raw string as the content.
    return { content: goal.agent_result };
  }
}

/**
 * Parse the `agent_progress` JSON string on a goal.
 */
export interface AgentProgress {
  message: string;
  detail?: string | null;
}

export function parseAgentProgress(goal: Goal): AgentProgress | null {
  if (!goal.agent_progress) return null;
  try {
    return JSON.parse(goal.agent_progress) as AgentProgress;
  } catch {
    return null;
  }
}

/**
 * Human-readable label for an agent status.
 */
export function agentStatusLabel(status: Goal['agent_status']): string {
  switch (status) {
    case 'queued':
      return 'Queued';
    case 'running':
      return 'Running…';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    default:
      return '';
  }
}
