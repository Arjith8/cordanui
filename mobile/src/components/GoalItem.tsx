import { useState } from 'react';
import { Pressable, StyleSheet, Text, TouchableOpacity, View } from 'react-native';
import DraggableFlatList, { type DragEndParams } from 'react-native-draggable-flatlist';

import { useTheme } from '@/theme/ThemeProvider';
import { statusColor } from '@/theme/types';
import type { Goal, GoalStatus } from '@/types/goal';
import { orderGoals } from '@/utils/order';

import AgentStatusBadge from '@/components/AgentStatusBadge';
import EditableText from '@/components/EditableText';
import InlineAddInput from '@/components/InlineAddInput';
import StatusCircle from '@/components/StatusCircle';
import PluginCard from '@/components/PluginCard';
import DynamicForm from '@/components/DynamicForm';
import { parseMobileWidgets } from '@/plugin/metadata';
import { parseAssignContext, parseDynamicForm } from '@/plugin/dynamicForm';

/**
 * A goal rendered as an accordion node in a border-drawn tree. Children live
 * in their own nested DraggableFlatList: long-press (350ms) a row to pick it
 * up and drag it among its siblings.
 */
export interface GoalItemProps {
  goal: Goal;
  childrenMap: Map<string | null, Goal[]>;
  prefix: boolean[];
  isLast: boolean;
  dragging?: boolean;
  /** From DraggableFlatlist's renderItem — starts a drag when long-pressed. */
  drag?: () => void;
  onLongPress?: (goal: Goal) => void;
  onCycleStatus: (id: string) => void;
  onSetStatus: (id: string, status: GoalStatus) => void;
  onSubmitSubgoal: (parentId: string, sheetId: string | null) => void;
  onRename: (id: string, title: string) => void;
  onSaveDescription: (id: string, description: string) => void;
  onDeleteDirect: (id: string) => void;
  onMove?: (id: string) => void;
  /** Persist a sibling group's new order. */
  onReorderGroup: (group: Goal[]) => void;
  /**
   * Single-active subgoal draft: the text typed into THIS node's input
   * ('' when inactive), plus focus/typing handlers owned by the screen so
   * only one subgoal input can hold a draft at any time.
   */
  draftText: string;
  /** Focus/typing handlers keyed by parent id — the screen owns the single draft. */
  onDraftFocus: (parentId: string) => void;
  onDraftChange: (parentId: string, text: string) => void;
  /** Returns the draft text for a node ('' when inactive). */
  draftFor: (id: string) => string;
}

const STATUS_OPTIONS: { status: GoalStatus; glyph: string; label: string }[] = [
  { status: 'pending', glyph: '○', label: 'Pending' },
  { status: 'in_progress', glyph: '◐', label: 'WIP' },
  { status: 'completed', glyph: '●', label: 'Done' },
];

/** One ancestor level: an empty cell, or a continuing vertical line. */
function Guide({ cont, color }: { cont: boolean; color: string }) {
  return (
    <View style={[styles.guideCell, cont && { borderLeftWidth: 2, borderLeftColor: color }]} />
  );
}

function guideKeys(prefix: boolean[]): string[] {
  return prefix.map((_, i) => `guide-${i}`);
}

/**
 * The ├─ / └─ connector drawn with views: a vertical segment through the
 * row's left edge (stopping at mid-height for last children) plus a stub.
 */
function Elbow({ isLast, color }: { isLast: boolean; color: string }) {
  return (
    <View style={styles.elbow}>
      <View style={[styles.elbowV, isLast && styles.elbowVHalf, { backgroundColor: color }]} />
      <View style={[styles.elbowH, { backgroundColor: color }]} />
    </View>
  );
}

/** Guide cells matching this node's own depth, used to indent its body. */
function BodyGuides({ prefix, color }: { prefix: boolean[]; color: string }) {
  const keys = guideKeys(prefix);
  return (
    <>
      {prefix.map((cont, i) => (
        <Guide key={keys[i]} cont={cont} color={color} />
      ))}
    </>
  );
}

export default function GoalItem({
  goal,
  childrenMap,
  prefix,
  isLast,
  dragging = false,
  drag,
  onLongPress,
  onCycleStatus,
  onSetStatus,
  onSubmitSubgoal,
  onRename,
  onSaveDescription,
  onDeleteDirect,
  onMove,
  onReorderGroup,
  draftText,
  onDraftFocus,
  onDraftChange,
  draftFor,
}: GoalItemProps) {
  const { colors } = useTheme();
  const [open, setOpen] = useState(false);

  const children = orderGoals(childrenMap.get(goal.id) ?? []);
  const completed = goal.status === 'completed';

  const submitSubgoal = () => {
    if (!draftText.trim()) return;
    onSubmitSubgoal(goal.id, goal.sheet_id);
  };

  const handleChildDragEnd = (params: DragEndParams<Goal>) => onReorderGroup(params.data);

  return (
    <View style={[dragging && styles.dragging]}>
      {/* Header row */}
      <View style={[styles.header, completed && styles.dimmed]}>
        <BodyGuides prefix={prefix} color={colors.outlineVariant} />
        <Elbow isLast={isLast} color={colors.outlineVariant} />
        <StatusCircle status={goal.status} onPress={() => onCycleStatus(goal.id)} />
        <View style={styles.titleArea}>
          <EditableText
            value={goal.title}
            onTap={() => setOpen((v) => !v)}
            onEditStart={() => setOpen(true)}
            onCommit={(t) => onRename(goal.id, t)}
            style={[styles.title, { color: colors.onBackground }, completed && styles.titleDone]}
            placeholderColor={colors.outlineVariant}
          />
        </View>
        {drag ? (
          <TouchableOpacity
            onPressIn={undefined}
            onLongPress={drag}
            hitSlop={6}
            style={styles.grip}
          >
            <Text style={{ color: colors.outlineVariant, fontSize: 16 }}>≡</Text>
          </TouchableOpacity>
        ) : null}
        <Text style={[styles.chevron, { color: colors.outlineVariant }]}>
          {children.length > 0 || open ? (open ? '▾' : '▸') : ''}
        </Text>
      </View>

      {/* Accordion body */}
      {open ? (
        <View>
          {/* Goal's own content, guided by its ancestors' lines */}
          <View style={[styles.bodyRow, completed && styles.dimmed]}>
            <BodyGuides prefix={prefix} color={colors.outlineVariant} />
            <View style={styles.bodySpacer} />
            <View style={styles.bodyContent}>
              <EditableText
                value={goal.description ?? ''}
                multiline
                emptyLabel="No description — double-tap to add one"
                placeholderColor={colors.outlineVariant}
                onCommit={(text) => onSaveDescription(goal.id, text)}
                style={[
                  goal.description ? styles.description : styles.descriptionEmpty,
                  { color: goal.description ? colors.onSurfaceVariant : colors.outlineVariant },
                ]}
              />

              {/* Status setter + delete */}
              <View style={styles.actionsRow}>
                <View style={styles.statusPicker}>
                  {STATUS_OPTIONS.map((opt) => {
                    const activeOpt = goal.status === opt.status;
                    return (
                      <Pressable
                        key={opt.status}
                        onPress={() => onSetStatus(goal.id, opt.status)}
                        hitSlop={6}
                        style={[
                          styles.statusOption,
                          { borderColor: colors.outline },
                          activeOpt && { borderColor: statusColor(colors, opt.status) },
                        ]}
                      >
                        <Text style={{ color: statusColor(colors, opt.status), fontSize: 13 }}>
                          {opt.glyph}
                        </Text>
                        <Text
                          style={[
                            styles.statusOptionLabel,
                            {
                              color: activeOpt
                                ? statusColor(colors, opt.status)
                                : colors.onSurfaceVariant,
                              fontWeight: activeOpt ? ('600' as const) : ('400' as const),
                            },
                          ]}
                        >
                          {opt.label}
                        </Text>
                      </Pressable>
                    );
                  })}
                </View>
                <View style={{ flexDirection: 'row', gap: 12, alignItems: 'center' }}>
                  {onMove ? (
                    <Pressable onPress={() => onMove(goal.id)} hitSlop={8}>
                      <Text style={{ color: colors.primary, fontSize: 13 }}>↳ Move</Text>
                    </Pressable>
                  ) : null}
                  <Pressable onPress={() => onDeleteDirect(goal.id)} hitSlop={8}>
                    <Text style={{ color: colors.error, fontSize: 15 }}>🗑</Text>
                  </Pressable>
                </View>
              </View>

              {/* Due / reminder / repeat — first-class, not plugin */}
              {(goal.due_at || goal.remind_at || goal.repeat_rule) && (
                <View style={{ flexDirection: 'row', flexWrap: 'wrap', gap: 6, marginTop: 8 }}>
                  {goal.due_at ? (
                    <Text
                      style={{
                        fontSize: 12,
                        paddingHorizontal: 6,
                        paddingVertical: 2,
                        borderRadius: 6,
                        backgroundColor: colors.surfaceVariant,
                        color: goal.due_at < new Date().toISOString() && goal.status !== 'completed' ? colors.error : colors.primary,
                      }}
                    >
                      📅 {goal.due_at.slice(0, 10)}
                    </Text>
                  ) : null}
                  {goal.remind_at ? (
                    <Text
                      style={{
                        fontSize: 12,
                        paddingHorizontal: 6,
                        paddingVertical: 2,
                        borderRadius: 6,
                        backgroundColor: colors.surfaceVariant,
                        color: colors.tertiary,
                      }}
                    >
                      ⏰ {goal.remind_at.slice(0, 16).replace('T', ' ')}
                    </Text>
                  ) : null}
                  {goal.repeat_rule ? (
                    <Text
                      style={{
                        fontSize: 12,
                        paddingHorizontal: 6,
                        paddingVertical: 2,
                        borderRadius: 6,
                        backgroundColor: colors.surfaceVariant,
                        color: colors.onSurfaceVariant,
                      }}
                    >
                      ↻ {goal.repeat_rule}
                    </Text>
                  ) : null}
                </View>
              )}

              {/* Agent status badge — only for goals in agent_mode. */}
              {goal.status === 'agent_mode' ? (
                <AgentStatusBadge goal={goal} />
              ) : null}

              {/* Plugin-driven card — any plugin can inject declarative widgets
                  via goals.metadata JSON (mobile.card / mobile.widgets). Host
                  owns rendering; plugins never run code on device. */}
              {(() => {
                const widgets = parseMobileWidgets(goal);
                return widgets ? <PluginCard widgets={widgets} /> : null;
              })()}
              {/* Dynamic form via metadata.data.form — any plugin can expose a
                  form schema (fields) without code push; assign_context too. */}
              {(() => {
                const form = parseDynamicForm(goal);
                return form ? <DynamicForm schema={form} /> : null;
              })()}
              {(() => {
                const ctx = parseAssignContext(goal);
                return ctx ? (
                  <View style={{ marginTop: 8, padding: 8, backgroundColor: colors.surfaceVariant, borderRadius: 8 }}>
                    <Text style={{ fontSize: 11, color: colors.tertiary, fontWeight: '600' }}>Assign context</Text>
                    <Text style={{ fontSize: 13, color: colors.onSurface, marginTop: 4 }} selectable>
                      {ctx}
                    </Text>
                  </View>
                ) : null;
              })()}
            </View>
          </View>

          {/* Children — nested draggable group */}
          {children.length > 0 ? (
            <DraggableFlatList
              data={children}
              scrollEnabled={false}
              keyExtractor={(item) => item.id}
              onDragEnd={handleChildDragEnd}
              renderItem={({ item, drag, isActive }) => (
                <GoalItem
                  goal={item}
                  childrenMap={childrenMap}
                  prefix={[...prefix, !isLast]}
                  isLast={(children.indexOf(item) ?? 0) === children.length - 1}
                  dragging={isActive}
                  drag={drag}
                  onLongPress={onLongPress}
                  onCycleStatus={onCycleStatus}
                  onSetStatus={onSetStatus}
                  onSubmitSubgoal={onSubmitSubgoal}
                  onRename={onRename}
                  onSaveDescription={onSaveDescription}
                  onDeleteDirect={onDeleteDirect}
                  onMove={onMove}
                  onReorderGroup={onReorderGroup}
                  draftText={draftFor(item.id)}
                  draftFor={draftFor}
                  onDraftFocus={onDraftFocus}
                  onDraftChange={onDraftChange}
                />
              )}
            />
          ) : null}

          {/* Add-subgoal input, aligned under the children column */}
          <View style={[styles.bodyRow, completed && styles.dimmed]}>
            <BodyGuides prefix={prefix} color={colors.outlineVariant} />
            <View style={styles.bodySpacer} />
            <InlineAddInput
              nested
              value={draftText}
              onChangeText={(text) => onDraftChange(goal.id, text)}
              onFocus={() => onDraftFocus(goal.id)}
              onSubmit={submitSubgoal}
              placeholder="Add subgoal…"
            />
          </View>
        </View>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  dragging: {
    opacity: 0.85,
    backgroundColor: 'rgba(59,130,246,0.12)',
    borderRadius: 8,
  },
  header: {
    flexDirection: 'row',
    alignItems: 'center',
    paddingRight: 8,
  },
  dimmed: {
    opacity: 0.55,
  },

  guideCell: {
    width: 18,
    alignSelf: 'stretch',
  },
  elbow: {
    width: 24,
    height: 32,
  },
  elbowV: {
    position: 'absolute',
    left: 0,
    top: 0,
    bottom: 0,
    width: 2,
  },
  elbowVHalf: {
    position: 'absolute',
    left: 0,
    top: 0,
    bottom: '50%',
    width: 2,
  },
  elbowH: {
    position: 'absolute',
    left: 0,
    top: '50%',
    marginTop: -1,
    width: 16,
    height: 2,
  },

  titleArea: {
    flex: 1,
    marginLeft: 2,
    paddingVertical: 6,
  },
  title: {
    fontSize: 16,
  },
  titleDone: {
    textDecorationLine: 'line-through',
  },
  chevron: {
    fontSize: 12,
    paddingHorizontal: 8,
  },
  grip: {
    width: 26,
    height: 32,
    justifyContent: 'center',
    alignItems: 'center',
  },

  bodyRow: {
    flexDirection: 'row',
  },
  bodySpacer: {
    width: 24,
  },
  bodyContent: {
    flex: 1,
    paddingRight: 12,
  },
  description: {
    fontSize: 14,
    marginTop: 4,
    lineHeight: 20,
  },
  descriptionEmpty: {
    fontSize: 13,
    fontStyle: 'italic',
    marginTop: 4,
  },
  actionsRow: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    marginVertical: 10,
  },
  statusPicker: {
    flexDirection: 'row',
    gap: 8,
  },
  statusOption: {
    flexDirection: 'row',
    alignItems: 'center',
    gap: 5,
    borderWidth: 1,
    borderRadius: 999,
    paddingHorizontal: 10,
    paddingVertical: 4,
  },
  statusOptionLabel: {
    fontSize: 12,
  },
});
