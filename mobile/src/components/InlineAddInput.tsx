import { Pressable, StyleSheet, Text, TextInput, View } from 'react-native';

import { useTheme } from '@/theme/ThemeProvider';

/**
 * Text input whose "Add" action only appears once there is something to add.
 */
export default function InlineAddInput({
  value,
  onChangeText,
  onSubmit,
  placeholder,
  autoFocus,
  onCancel,
  nested,
  onFocus,
}: {
  value: string;
  onChangeText: (text: string) => void;
  onSubmit: () => void;
  placeholder: string;
  autoFocus?: boolean;
  onCancel?: () => void;
  onFocus?: () => void;
  /** In-tree variant: no outer padding/divider so tree lines stay continuous. */
  nested?: boolean;
}) {
  const { colors } = useTheme();
  const canAdd = value.trim().length > 0;
  return (
    <View style={[styles.rowInner, nested ? styles.rowNested : styles.rowOuter]}>
      <TextInput
        style={[
          styles.input,
          {
            backgroundColor: colors.surface,
            color: colors.onBackground,
            borderColor: colors.outline,
          },
        ]}
        value={value}
        onChangeText={onChangeText}
        placeholder={placeholder}
        placeholderTextColor={colors.outlineVariant}
        onSubmitEditing={() => canAdd && onSubmit()}
        returnKeyType="done"
        autoFocus={autoFocus}
        onFocus={onFocus}
      />
      {canAdd ? (
        <Pressable
          onPress={onSubmit}
          hitSlop={8}
          style={[styles.addBtn, { backgroundColor: colors.primary }]}
        >
          <Text style={[styles.addBtnText, { color: colors.onPrimary }]}>Add</Text>
        </Pressable>
      ) : null}
      {onCancel ? (
        <Pressable onPress={onCancel} hitSlop={8}>
          <Text style={[styles.cancelBtn, { color: colors.onSurfaceVariant }]}>✕</Text>
        </Pressable>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  rowOuter: {
    paddingHorizontal: 16,
    paddingVertical: 10,
    gap: 10,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  rowInner: {
    flexDirection: 'row',
    alignItems: 'center',
    gap: 8,
  },
  rowNested: {
    // Inside a horizontal tree-body row: expand to fill remaining width,
    // otherwise the flex:1 input collapses to zero.
    flex: 1,
  },
  input: {
    flex: 1,
    borderRadius: 8,
    borderWidth: 1,
    paddingHorizontal: 12,
    paddingVertical: 8,
    fontSize: 15,
  },
  addBtn: {
    borderRadius: 8,
    paddingHorizontal: 14,
    paddingVertical: 8,
  },
  addBtnText: {
    fontSize: 14,
    fontWeight: '600',
  },
  cancelBtn: {
    fontSize: 16,
    paddingHorizontal: 4,
  },
});
