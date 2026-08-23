import type { Goal } from '@/types/goal';

/**
 * Active goals first (insertion order), completed ones sink to the bottom.
 * Used at every tree level so finished work de-escalates out of focus.
 */
export function orderGoals(goals: Goal[]): Goal[] {
  const cmp = (a: Goal, b: Goal) =>
    a.sort_order - b.sort_order || a.created_at.localeCompare(b.created_at);
  const active = goals.filter((g) => g.status !== 'completed').sort(cmp);
  const done = goals.filter((g) => g.status === 'completed').sort(cmp);
  return [...active, ...done];
}
