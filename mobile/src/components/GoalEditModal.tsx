import { useState } from 'react';
import { Modal, Pressable, StyleSheet, Text, TextInput, View } from 'react-native';

import { useTheme } from '@/theme/ThemeProvider';
import type { Goal } from '@/types/goal';

export interface GoalEditModalProps {
  goal: Goal | null;
  visible: boolean;
  onClose: () => void;
  onSave: (id: string, title: string, description: string) => void;
  onDelete: (id: string) => void;
}

export default function GoalEditModal({
  goal,
  visible,
  onClose,
  onSave,
  onDelete,
}: GoalEditModalProps) {
  const { colors } = useTheme();
  const [title, setTitle] = useState('');
  const [description, setDescription] = useState('');

  // Reset local state whenever a new goal is opened.
  const [lastId, setLastId] = useState<string | null>(null);
  if (goal && goal.id !== lastId) {
    setLastId(goal.id);
    setTitle(goal.title);
    setDescription(goal.description ?? '');
  }

  // No goal selected — render an inert modal shell.
  if (!goal) {
    return <Modal visible={false} animationType="slide" transparent onRequestClose={onClose} />;
  }

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
                if (title.trim()) onSave(goal.id, title.trim(), description.trim());
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
});
