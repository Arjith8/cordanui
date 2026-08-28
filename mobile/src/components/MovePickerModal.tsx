import { FlatList, Modal, Pressable, StyleSheet, Text, View } from 'react-native';

import { useTheme } from '@/theme/ThemeProvider';
import type { Goal } from '@/types/goal';

export interface MovePickerModalProps {
  visible: boolean;
  goal: Goal | null;
  goals: Goal[];
  onSelectParent: (newParentId: string | null) => void;
  onClose: () => void;
}

function getDescendantIds(goalId: string, allGoals: Goal[]): Set<string> {
  const byParent = new Map<string | null, Goal[]>();
  for (const g of allGoals) {
    const list = byParent.get(g.parent_id) ?? [];
    list.push(g);
    byParent.set(g.parent_id, list);
  }
  const out = new Set<string>();
  const stack = [goalId];
  while (stack.length > 0) {
    const cur = stack.pop()!;
    const children = byParent.get(cur) ?? [];
    for (const c of children) {
      if (!out.has(c.id)) {
        out.add(c.id);
        stack.push(c.id);
      }
    }
  }
  return out;
}

export default function MovePickerModal({ visible, goal, goals, onSelectParent, onClose }: MovePickerModalProps) {
  const { colors } = useTheme();
  if (!goal) return null;

  const descendants = getDescendantIds(goal.id, goals);
  const candidates: Array<{ id: string | null; label: string }> = [{ id: null, label: '∅  (root)' }];
  for (const g of goals) {
    if (g.id === goal.id || descendants.has(g.id)) continue;
    const glyph = g.status === 'completed' ? '✓' : g.status === 'in_progress' ? '◐' : g.status === 'agent_mode' ? '⤴' : '○';
    candidates.push({ id: g.id, label: `${glyph} ${g.title}` });
  }

  const currentParent = goal.parent_id;

  return (
    <Modal visible={visible} animationType="slide" transparent onRequestClose={onClose}>
      <View style={styles.backdrop}>
        <View style={[styles.sheet, { backgroundColor: colors.surface, borderColor: colors.outline }]}>
          <Text style={[styles.title, { color: colors.onSurface }]}>Move “{goal.title}”</Text>
          <Text style={[styles.subtitle, { color: colors.onSurfaceVariant }]}>Choose new parent — root or another goal</Text>
          <FlatList
            data={candidates}
            keyExtractor={(item) => item.id ?? 'root'}
            renderItem={({ item }) => {
              const active = item.id === currentParent;
              return (
                <Pressable
                  onPress={() => onSelectParent(item.id)}
                  style={[styles.row, active && { backgroundColor: colors.surfaceVariant }]}
                >
                  <Text style={[styles.rowLabel, { color: active ? colors.primary : colors.onSurface }]} numberOfLines={1}>
                    {item.label}
                  </Text>
                  {active ? <Text style={{ color: colors.primary, fontWeight: '600' }}> — current</Text> : null}
                </Pressable>
              );
            }}
            style={styles.list}
          />
          <Pressable onPress={onClose} style={[styles.cancel, { borderColor: colors.outline }]}>
            <Text style={{ color: colors.onSurfaceVariant }}>Cancel</Text>
          </Pressable>
        </View>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  backdrop: {
    flex: 1,
    backgroundColor: 'rgba(0,0,0,0.4)',
    justifyContent: 'flex-end',
  },
  sheet: {
    borderTopLeftRadius: 16,
    borderTopRightRadius: 16,
    borderWidth: 1,
    borderBottomWidth: 0,
    padding: 16,
    maxHeight: '80%',
  },
  title: {
    fontSize: 16,
    fontWeight: '600',
  },
  subtitle: {
    fontSize: 13,
    marginTop: 4,
    marginBottom: 12,
  },
  list: {
    flexGrow: 0,
  },
  row: {
    flexDirection: 'row',
    alignItems: 'center',
    paddingVertical: 12,
    paddingHorizontal: 8,
    borderRadius: 8,
  },
  rowLabel: {
    fontSize: 14,
    flex: 1,
  },
  cancel: {
    marginTop: 12,
    borderWidth: 1,
    borderRadius: 999,
    paddingVertical: 10,
    alignItems: 'center',
  },
});
