/**
 * Plugin-driven metadata → mobile widget bridge.
 *
 * Goals carry `metadata` TEXT (JSON, synced via Turso). Plugins (agent / provider
 * or any future plugin) can write a `mobile` key that declares declarative
 * widgets for the mobile client to render — the same vocabulary the TUI's
 * `cord.ui.show_panel` uses, so a plugin can ship once and target both hosts.
 *
 * Accepted shapes (all optional, all tolerant):
 *
 * ```json
 * {
 *   "agent": "my-agent",
 *   "mobile": {
 *     "card": { "content": "hello", "fg": "primary", "bold": true }
 *     // or: { "items": ["a","b"], "highlight": 1 }
 *     // or: { "children": [ widget, widget ] }
 *     // or: [ widget, widget ]  // array is a vertical stack
 *   }
 * }
 * ```
 *
 * Legacy / shorthand aliases also read:
 * - top-level `mobile_card` / `mobile_widget` / `card` (object or array)
 * - `mobile.widgets` (alias for `mobile.card`)
 *
 * Widgets are deliberately data-only — no code, no event handlers. The host
 * (GoalItem) renders them as read-only cards.
 */

import type { Goal } from '@/types/goal';

export type TextWidget = { content: string; fg?: string; bold?: boolean };
export type ListWidget = { items: string[]; highlight?: number };
export type ColumnWidget = { children: Widget[] };
export type Widget = TextWidget | ListWidget | ColumnWidget;

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function isTextWidget(v: unknown): v is TextWidget {
  if (!isRecord(v)) return false;
  return typeof v.content === 'string';
}

function isListWidget(v: unknown): v is ListWidget {
  if (!isRecord(v)) return false;
  const items = v.items;
  if (!Array.isArray(items)) return false;
  return items.every((x) => typeof x === 'string');
}

function isColumnWidget(v: unknown): v is ColumnWidget {
  if (!isRecord(v)) return false;
  return Array.isArray(v.children);
}

function normalizeWidget(v: unknown): Widget | null {
  if (isTextWidget(v)) return v as TextWidget;
  if (isListWidget(v)) return v as ListWidget;
  if (isColumnWidget(v)) {
    const raw = (v as Record<string, unknown>).children as unknown[];
    const children = raw.map(normalizeWidget).filter((x): x is Widget => x !== null);
    return { children };
  }
  return null;
}

function normalizeWidgetArray(v: unknown): Widget[] | null {
  if (Array.isArray(v)) {
    const out = v.map(normalizeWidget).filter((x): x is Widget => x !== null);
    return out.length > 0 ? out : null;
  }
  const single = normalizeWidget(v);
  return single ? [single] : null;
}

/**
 * Extract a plugin-declared widget tree from a goal's metadata.
 * Returns null if absent or invalid (never throws).
 */
export function parseMobileWidgets(goal: Goal): Widget[] | null {
  if (!goal.metadata) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(goal.metadata);
  } catch {
    return null;
  }
  if (!isRecord(parsed)) return null;

  // Preferred: parsed.mobile.card / parsed.mobile.widgets / parsed.mobile itself as widget/array
  const mobile = parsed.mobile as unknown;
  if (mobile !== undefined) {
    if (isRecord(mobile)) {
      // mobile.card or mobile.widgets
      const card =
        (mobile as Record<string, unknown>).card ?? (mobile as Record<string, unknown>).widgets;
      if (card !== undefined) {
        const w = normalizeWidgetArray(card);
        if (w) return w;
      }
      // mobile itself might be a widget/array (shorthand)
      const direct = normalizeWidgetArray(mobile);
      // Avoid treating {"card":...} object itself as a widget — already handled
      if (
        direct &&
        !(
          isRecord(mobile) &&
          ('card' in (mobile as Record<string, unknown>) ||
            'widgets' in (mobile as Record<string, unknown>))
        )
      ) {
        // If mobile was a text/list/column widget directly, direct will be non-null
        // but we don't want to double-count plain objects like {card:...} that normalized to null
        // The guard above ensures we only return direct when there was no card/widgets key
        return direct;
      }
      // If mobile had card/widgets but it was invalid, fall through to legacy aliases
    } else if (Array.isArray(mobile) || isTextWidget(mobile) || isListWidget(mobile)) {
      const w = normalizeWidgetArray(mobile);
      if (w) return w;
    }
  }

  // Legacy shorthands at top level
  for (const key of ['mobile_card', 'mobile_widget', 'card', 'widget']) {
    const val = (parsed as Record<string, unknown>)[key];
    if (val !== undefined) {
      const w = normalizeWidgetArray(val);
      if (w) return w;
    }
  }

  return null;
}

/**
 * Human label for the active agent/provider stored in metadata, if any.
 */
export function parseAgentMeta(goal: Goal): { agent: string | null; model: string | null } {
  if (!goal.metadata) return { agent: null, model: null };
  try {
    const p = JSON.parse(goal.metadata) as Record<string, unknown>;
    const agent = (p.agent as string) ?? (p.provider as string) ?? null;
    const model = (p.model as string) ?? null;
    return {
      agent: typeof agent === 'string' ? agent : null,
      model: typeof model === 'string' ? model : null,
    };
  } catch {
    return { agent: null, model: null };
  }
}
