import { useEffect, useState } from 'react';
import { Alert, Modal, Pressable, StyleSheet, Text, TextInput, View } from 'react-native';

import { useTheme } from '@/theme/ThemeProvider';
import type { Goal } from '@/types/goal';
import { assignToAgent, getAgentUrl, isAgentAvailable } from '@/db/agentDb';

export interface GoalEditModalProps {
  goal: Goal | null;
  visible: boolean;
  onClose: () => void;
  onSave: (id: string, title: string, description: string, dueAt: string | null, remindAt: string | null, repeatRule: string | null) => void;
  onDelete: (id: string) => void;
  /** Called after a goal is assigned to the agent (to trigger a refresh). */
  onAgentAssigned?: (id: string) => void;
}

export default function GoalEditModal({
  goal,
  visible,
  onClose,
  onSave,
  onDelete,
  onAgentAssigned,
}: GoalEditModalProps) {
  const { colors } = useTheme();
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');
  const [dueAt, setDueAt] = useState('');
  const [remindAt, setRemindAt] = useState('');
  const [repeatRule, setRepeatRule] = useState('');
  const [agentAvailable, setAgentAvailable] = useState(false);

  // Reset local state whenever a new goal is opened.
  const [lastId, setLastId] = useState<string | null>(null);
  if (goal && goal.id !== lastId) {
    setLastId(goal.id);
    setTitle(goal.title);
    setDescription(goal.description ?? '');
    setDueAt(goal.due_at ?? '');
    setRemindAt(goal.remind_at ?? '');
    setRepeatRule(goal.repeat_rule ?? '');
  }

  // Check agent availability when the modal opens for a new goal.
  useEffect(() => {
    if (visible && goal) {
      isAgentAvailable().then(setAgentAvailable);
    }
  }, [visible, goal]);

  // No goal selected — render an inert modal shell.
  if (!goal) {
    return <Modal visible={false} animationType="slide" transparent onRequestClose={onClose} />;
  }

  const handleAssign = () => {
    Alert.alert(
      'Assign to agent',
      'The agent backend will process this goal and write the result back. You can track progress here.',
      [
        { text: 'Cancel', style: 'cancel' },
        {
          text: 'Assign',
          onPress: async () => {
            await assignToAgent(goal.id);
            onAgentAssigned?.(goal.id);
            onClose();
          },
        },
      ],
    );
  };

  return (
    <Modal visible={visible} animationType="slide" transparent onRequestClose={onClose}>
      <View style={[styles.overlay, { backgroundColor: `${colors.outlineVariant}80` }]}>
        <View style={[styles.sheet, { backgroundColor: colors.background }]}>
          <Text style={[styles.header, { color: colors.onBackground }]}>Edit goal</Text>

          <Text style={[styles.label, { color: colors.outlineVariant }]}>Title</Text>
          <TextInput
            style={[styles.input, { backgroundColor: colors.surface, color: colors.onBackground }]}
            value={title}
            onChangeText={setTitle}
            placeholder="Goal title"
            placeholderTextColor={colors.outlineVariant}
            autoFocus
          />

          <Text style={[styles.label, { color: colors.outlineVariant }]}>Description</Text>
          <TextInput
            style={[
              styles.input,
              styles.multiline,
              { backgroundColor: colors.surface, color: colors.onBackground },
            ]}
            value={description}
            onChangeText={setDescription}
            placeholder="Optional description"
            placeholderTextColor={colors.outlineVariant}
            multiline
            numberOfLines={4}
          />

          <Text style={[styles.label, { color: colors.outlineVariant }]}>Due (YYYY-MM-DD)</Text>
          <TextInput
            style={[styles.input, { backgroundColor: colors.surface, color: colors.onBackground }]}
            value={dueAt}
            onChangeText={setDueAt}
            placeholder="2026-09-01"
            placeholderTextColor={colors.outlineVariant}
            autoCapitalize="none"
            autoCorrect={false}
          />
          <Text style={[styles.label, { color: colors.outlineVariant }]}>Remind at (ISO)</Text>
          <TextInput
            style={[styles.input, { backgroundColor: colors.surface, color: colors.onBackground }]}
            value={remindAt}
            onChangeText={setRemindAt}
            placeholder="2026-09-01T09:00:00Z"
            placeholderTextColor={colors.outlineVariant}
            autoCapitalize="none"
            autoCorrect={false}
          />
          <Text style={[styles.label, { color: colors.outlineVariant }]}>Repeat</Text>
          <View style={styles.repeatRow}>
            {(['none', 'daily', 'weekly', 'monthly', 'yearly'] as const).map((opt) => (
              <Pressable
                key={opt}
                onPress={() => setRepeatRule(opt === 'none' ? '' : opt)}
                style={[
                  styles.repeatChip,
                  { borderColor: colors.outline },
                  (repeatRule || 'none') === opt || (opt === 'none' && !repeatRule) ? { backgroundColor: colors.primary, borderColor: colors.primary } : null,
                ]}
              >
                <Text style={{ color: (repeatRule || 'none') === opt || (opt === 'none' && !repeatRule) ? colors.onPrimary : colors.onSurfaceVariant, fontSize: 12 }}>{opt}</Text>
              </Pressable>
            ))}
          </View>

          {agentAvailable && goal.status !== 'agent_mode' ? (
            <Pressable
              style={[styles.agentBtn, { backgroundColor: colors.tertiary }]}
              onPress={handleAssign}
            >
              <Text style={[styles.agentBtnText, { color: colors.onTertiary }]}>
                ⤴ Assign to agent
              </Text>
            </Pressable>
          ) : null}

          {goal.status === 'agent_mode' ? (
            <View style={[styles.agentStatus, { backgroundColor: colors.surfaceVariant }]}>
              <Text style={[styles.agentStatusText, { color: colors.tertiary }]}>
                ⤴ Agent {goal.agent_status ?? 'queued'}
              </Text>
              {goal.agent_result ? (
                <Text style={[styles.agentResult, { color: colors.onSurfaceVariant }]}>
                  {(() => {
                    try {
                      const r = JSON.parse(goal.agent_result);
                      return typeof r === 'string' ? r : r.content ?? JSON.stringify(r);
                    } catch {
                      return goal.agent_result;
                    }
                  })()}
                </Text>
              ) : null}
            </View>
          ) : null}

          <View style={styles.actions}>
            <Pressable style={styles.deleteBtn} onPress={() => onDelete(goal.id)}>
              <Text style={[styles.deleteText, { color: colors.error }]}>Delete</Text>
            </Pressable>
            <View style={{ flex: 1 }} />
            <Pressable style={styles.cancelBtn} onPress={onClose}>
              <Text style={[styles.cancelText, { color: colors.onSurfaceVariant }]}>Cancel</Text>
            </Pressable>
            <Pressable
              style={[styles.saveBtn, { backgroundColor: colors.primary }]}
              onPress={() => {
                if (title.trim())
                  onSave(
                    goal.id,
                    title.trim(),
                    description.trim(),
                    dueAt.trim() || null,
                    remindAt.trim() || null,
                    repeatRule.trim() || null,
                  );
              }}
            >
              <Text style={[styles.saveText, { color: colors.onPrimary }]}>Save</Text>
            </Pressable>
          </View>
        </View>
      </View>
    </Modal>
  );
}

const styles = StyleSheet.create({
  overlay: {
    flex: 1,
    justifyContent: 'flex-end',
    backgroundColor: 'rgba(0,0,0,0.5)',
  },
  sheet: {
    borderTopLeftRadius: 16,
    borderTopRightRadius: 16,
    padding: 20,
    paddingBottom: 40,
  },
  header: {
    fontSize: 18,
    fontWeight: '600',
    marginBottom: 16,
  },
  label: {
    fontSize: 12,
    marginBottom: 6,
    marginTop: 12,
    textTransform: 'uppercase',
  },
  input: {
    borderRadius: 8,
    padding: 12,
    fontSize: 16,
  },
  multiline: {
    minHeight: 96,
    textAlignVertical: 'top',
  },
  agentBtn: {
    borderRadius: 8,
    paddingVertical: 12,
    paddingHorizontal: 16,
    alignItems: 'center',
    marginTop: 16,
  },
  agentBtnText: {
    fontSize: 15,
    fontWeight: '600',
  },
  agentStatus: {
    borderRadius: 8,
    padding: 12,
    marginTop: 16,
  },
  agentStatusText: {
    fontSize: 14,
    fontWeight: '600',
    marginBottom: 4,
  },
  agentResult: {
    fontSize: 13,
    lineHeight: 18,
  },
  actions: {
    flexDirection: 'row',
    alignItems: 'center',
    marginTop: 24,
    gap: 8,
  },
  deleteBtn: {
    paddingVertical: 10,
    paddingHorizontal: 14,
  },
  deleteText: {
    fontSize: 16,
  },
  cancelBtn: {
    paddingVertical: 10,
    paddingHorizontal: 14,
  },
  cancelText: {
    fontSize: 16,
  },
  saveBtn: {
    paddingVertical: 10,
    paddingHorizontal: 20,
    borderRadius: 8,
  },
  saveText: {
    fontSize: 16,
    fontWeight: '600',
  },
  repeatRow: {
    flexDirection: 'row',
    gap: 6,
    flexWrap: 'wrap',
    marginTop: 4,
  },
  repeatChip: {
    borderWidth: 1,
    borderRadius: 999,
    paddingHorizontal: 10,
    paddingVertical: 5,
  },
});
