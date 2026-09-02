/**
 * Dynamic form bridge — plugins expose JSON via goals.metadata.data
 * (written by cord.goals.set_data). Mobile renders it without code push.
 *
 * A plugin (e.g. select-assign) writes:
 *   metadata.data.form = {
 *     title: "Select-assign batch",
 *     fields: [
 *       {key:"context", label:"Extra context", type:"text", value:"..."},
 *       {key:"model", label:"Model", type:"select", value:"big-pickle", options:["a","b"]},
 *       {key:"goals", label:"Goals", type:"list", value:["id1","id2"]}
 *     ]
 *   }
 * Host owns rendering; plugins never run code on device.
 */

import type { Goal } from '@/types/goal';

export type FormField = {
  key: string;
  label: string;
  type: 'text' | 'select' | 'list';
  value: unknown;
  options?: string[];
  required?: boolean;
};

export type FormSchema = {
  title?: string;
  fields: FormField[];
};

function isRecord(v: unknown): v is Record<string, unknown> {
  return typeof v === 'object' && v !== null && !Array.isArray(v);
}

function parseField(v: unknown): FormField | null {
  if (!isRecord(v)) return null;
  const key = v.key;
  const label = v.label;
  if (typeof key !== 'string' || typeof label !== 'string') return null;
  const type = (v.type as string) || 'text';
  if (!['text', 'select', 'list'].includes(type)) return null;
  const out: FormField = {
    key,
    label,
    type: type as FormField['type'],
    value: v.value,
    options: Array.isArray(v.options) ? (v.options as string[]).filter((x) => typeof x === 'string') : undefined,
    required: typeof v.required === 'boolean' ? v.required : undefined,
  };
  return out;
}

export function parseDynamicForm(goal: Goal): FormSchema | null {
  if (!goal.metadata) return null;
  let parsed: unknown;
  try {
    parsed = JSON.parse(goal.metadata);
  } catch {
    return null;
  }
  if (!isRecord(parsed)) return null;
  const data = parsed.data as unknown;
  if (!isRecord(data)) return null;
  const form = (data as Record<string, unknown>).form as unknown;
  if (!isRecord(form) && !Array.isArray(form)) return null;
  // form can be {fields:[...]} or directly {fields} as array shorthand?
  let fieldsRaw: unknown;
  let title: string | undefined;
  if (isRecord(form) && Array.isArray((form as Record<string, unknown>).fields)) {
    fieldsRaw = (form as Record<string, unknown>).fields;
    title = (form as Record<string, unknown>).title as string | undefined;
  } else if (Array.isArray(form)) {
    fieldsRaw = form;
  } else {
    return null;
  }
  if (!Array.isArray(fieldsRaw)) return null;
  const fields = fieldsRaw.map(parseField).filter((x): x is FormField => x !== null);
  if (fields.length === 0) return null;
  return { title: typeof title === 'string' ? title : undefined, fields };
}

export function parseAssignContext(goal: Goal): string | null {
  if (!goal.metadata) return null;
  try {
    const p = JSON.parse(goal.metadata) as Record<string, unknown>;
    const data = p.data as Record<string, unknown> | undefined;
    if (data && typeof data.assign_context === 'string' && (data.assign_context as string).trim()) {
      return data.assign_context as string;
    }
    // fallback: top-level assign_context
    if (typeof p.assign_context === 'string' && (p.assign_context as string).trim()) {
      return p.assign_context as string;
    }
  } catch {}
  return null;
}
