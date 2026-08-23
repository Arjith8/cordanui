import { useRef, useState } from 'react';
import { Pressable, StyleSheet, Text, TextInput } from 'react-native';
import type { StyleProp, TextStyle } from 'react-native';

/**
 * Text that behaves normally (tap → onTap) but enters an inline edit field
 * on double-tap. Used for goal/subgoal titles and descriptions so every
 * editable text in the tree shares one interaction pattern.
 */
export default function EditableText({
  value,
  onCommit,
  onTap,
  onEditStart,
  style,
  placeholderColor = '#94a3b8',
  emptyLabel,
  multiline = false,
}: {
  value: string;
  onCommit: (value: string) => void;
  /** Single tap. Fired ~280ms after the tap unless a double-tap follows. */
  onTap?: () => void;
  onEditStart?: () => void;
  style?: StyleProp<TextStyle>;
  placeholderColor?: string;
  emptyLabel?: string;
  multiline?: boolean;
}) {
  const [editing, setEditing] = useState(false);
  const [draft, setDraft] = useState(value);
  const lastTap = useRef(0);
  const tapTimer = useRef<ReturnType<typeof setTimeout> | null>(null);

  const startEditing = () => {
    setDraft(value);
    setEditing(true);
    onEditStart?.();
  };

  const handlePress = () => {
    const now = Date.now();
    if (now - lastTap.current < 300) {
      // Double-tap → edit (cancel the pending single-tap action).
      if (tapTimer.current) clearTimeout(tapTimer.current);
      lastTap.current = 0;
      startEditing();
      return;
    }
    lastTap.current = now;
    if (onTap) {
      tapTimer.current = setTimeout(() => {
        lastTap.current = 0;
        onTap();
      }, 280);
    }
  };

  const commit = () => {
    setEditing(false);
    if (!multiline) {
      const trimmed = draft.trim();
      if (trimmed && trimmed !== value) onCommit(trimmed);
      return;
    }
    if (draft !== value) onCommit(draft);
  };

  if (editing) {
    return (
      <TextInput
        style={style}
        value={draft}
        onChangeText={setDraft}
        onBlur={commit}
        onSubmitEditing={multiline ? undefined : commit}
        autoFocus
        multiline={multiline}
        placeholderTextColor={placeholderColor}
      />
    );
  }

  return (
    <Pressable onPress={handlePress}>
      <Text style={[style, !value && { color: placeholderColor, fontStyle: 'italic' }]}>
        {value || emptyLabel || 'Untitled'}
      </Text>
    </Pressable>
  );
}
