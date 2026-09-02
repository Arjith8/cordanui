import { StyleSheet, Text, View } from 'react-native';

import type { FormSchema } from '@/plugin/dynamicForm';
import { useTheme } from '@/theme/ThemeProvider';

export default function DynamicForm({ schema }: { schema: FormSchema }) {
  const { colors } = useTheme();
  return (
    <View style={[styles.card, { backgroundColor: colors.surfaceVariant, borderColor: colors.outline }]}>
      {schema.title ? (
        <Text style={[styles.title, { color: colors.primary }]}>{schema.title}</Text>
      ) : null}
      {schema.fields.map((f) => (
        <View key={f.key} style={styles.field}>
          <Text style={[styles.label, { color: colors.onSurfaceVariant }]}>{f.label}</Text>
          {f.type === 'text' ? (
            <Text style={[styles.value, { color: colors.onSurface }]} selectable>
              {String(f.value ?? '') || '—'}
            </Text>
          ) : f.type === 'select' ? (
            <View style={[styles.badge, { backgroundColor: colors.primary, borderColor: colors.primary }]}>
              <Text style={[styles.badgeText, { color: colors.onPrimary }]}>{String(f.value ?? '')}</Text>
            </View>
          ) : f.type === 'list' ? (
            <View style={styles.list}>
              {Array.isArray(f.value) && (f.value as string[]).length > 0 ? (
                (f.value as string[]).map((v, i) => (
                  <Text key={`${i}-${v}`} style={[styles.listItem, { color: colors.onSurface }]}>
                    • {v.slice(0, 8)}… {v}
                  </Text>
                ))
              ) : (
                <Text style={[styles.value, { color: colors.outlineVariant }]}>—</Text>
              )}
            </View>
          ) : null}
          {f.options && f.options.length > 0 && f.type === 'select' ? (
            <Text style={[styles.hint, { color: colors.outlineVariant }]}>options: {f.options.slice(0, 4).join(', ')}</Text>
          ) : null}
        </View>
      ))}
    </View>
  );
}

const styles = StyleSheet.create({
  card: {
    marginTop: 10,
    borderWidth: 1,
    borderRadius: 10,
    padding: 12,
    gap: 10,
  },
  title: {
    fontSize: 13,
    fontWeight: '700',
  },
  field: {
    gap: 4,
  },
  label: {
    fontSize: 11,
    fontWeight: '600',
    textTransform: 'uppercase',
    letterSpacing: 0.5,
  },
  value: {
    fontSize: 13,
    lineHeight: 18,
  },
  badge: {
    alignSelf: 'flex-start',
    borderWidth: 1,
    borderRadius: 999,
    paddingHorizontal: 10,
    paddingVertical: 4,
  },
  badgeText: {
    fontSize: 12,
    fontWeight: '600',
  },
  list: {
    gap: 2,
  },
  listItem: {
    fontSize: 12,
  },
  hint: {
    fontSize: 10,
  },
});
