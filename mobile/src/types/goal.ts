/**
 * Shared goal types — mirrors rust/schema/schema.sql.
 * This is the canonical mobile client type layer. The TUI (Rust) and the
 * agent backend (Rust) each have their own equivalent against the same
 * schema. The SQL schema is the single source of truth.
 */

export type GoalStatus = 'pending' | 'in_progress' | 'completed' | 'agent_mode';

export type AgentStatus = 'queued' | 'running' | 'completed' | 'failed';

export interface Goal {
  id: string;
  title: string;
  description: string | null;
  status: GoalStatus;
  parent_id: string | null;
  sheet_id: string | null;
  sort_order: number;
  created_at: string; // ISO 8601
  updated_at: string; // ISO 8601
  completed_at: string | null;
  agent_status: AgentStatus | null;
  agent_result: string | null;
  agent_progress: string | null;
  metadata: string | null; // JSON string
  deleted_at: string | null; // soft-delete tombstone (sync); NULL = active
}

export interface GoalSheet {
  id: string;
  name: string;
  sort_order: number;
  created_at: string; // ISO 8601
}

export interface CreateGoalInput {
  title: string;
  description?: string | null;
  parent_id?: string | null;
  sheet_id?: string | null;
  sort_order?: number;
}

export interface UpdateGoalInput {
  title?: string;
  description?: string | null;
  status?: GoalStatus;
  sort_order?: number;
  completed_at?: string | null;
  agent_status?: AgentStatus | null;
  agent_result?: string | null;
  agent_progress?: string | null;
  metadata?: string | null;
}

/**
 * A goal with its immediate children expanded. The HomeScreen renders a
 * flat-ish tree by walking this structure.
 */
export interface GoalTreeNode extends Goal {
  children: GoalTreeNode[];
}

/** Statuses the StatusCircle cycles through on tap. */
export const STATUS_CYCLE: GoalStatus[] = ['pending', 'in_progress', 'completed'];

export function nextStatus(status: GoalStatus): GoalStatus {
  const i = STATUS_CYCLE.indexOf(status);
  return STATUS_CYCLE[(i + 1) % STATUS_CYCLE.length];
}
