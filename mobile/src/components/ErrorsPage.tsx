import { useCallback, useEffect, useState } from 'react';
import { FlatList, Pressable, StyleSheet, Text, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import type { LoggedError } from '@/db/errorsDb';
import { clearErrors, getErrors } from '@/db/errorsDb';
import { useTheme } from '@/theme/ThemeProvider';

/**
 * Profile / diagnostics page: theme picker + every error the app has logged
 * on-device. Reachable via the header button on HomeScreen.
 */
export default function ErrorsPage({ onBack }: { onBack: () => void }) {
  const insets = useSafeAreaInsets();
  const { colors, mode, themes, activeThemeId, selectTheme } = useTheme();
  const [errors, setErrors] = useState<LoggedError[]>([]);
  const [loading, setLoading] = useState(true);
  const [showThemes, setShowThemes] = useState(false);

  const refresh = useCallback(async () => {
    setErrors(await getErrors());
    setLoading(false);
  }, []);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const handleClear = useCallback(async () => {
    await clearErrors();
    await refresh();
  }, [refresh]);

  return (
    <View
      style={[styles.container, { backgroundColor: colors.background, paddingTop: insets.top }]}
    >
      <View style={[styles.header, { borderBottomColor: colors.outline }]}>
        <Pressable onPress={onBack} hitSlop={8}>
          <Text style={[styles.back, { color: colors.primary }]}>← Back</Text>
        </Pressable>
        <Text style={[styles.title, { color: colors.onBackground }]}>Profile</Text>
        <Pressable onPress={handleClear} hitSlop={8}>
          <Text style={[styles.clear, { color: colors.error }]}>Clear</Text>
        </Pressable>
      </View>

      {/* Themes section */}
      <Pressable
        onPress={() => setShowThemes((v) => !v)}
        style={[styles.sectionRow, { borderBottomColor: colors.outline }]}
      >
        <Text style={[styles.sectionTitle, { color: colors.onBackground }]}>🎨 Themes</Text>
        <Text style={[styles.sectionMeta, { color: colors.outlineVariant }]}>
          {mode === 'system' ? 'System' : (themes.find((t) => t.id === activeThemeId)?.name ?? '')}{' '}
          {showThemes ? '▾' : '▸'}
        </Text>
      </Pressable>

      {showThemes ? (
        <View style={[styles.themeList, { borderBottomColor: colors.outline }]}>
          <Pressable
            onPress={() => selectTheme(null)}
            hitSlop={4}
            style={[
              styles.themeOption,
              mode === 'system' && { borderColor: colors.primary },
              { borderColor: colors.outline },
            ]}
          >
            <Text style={{ color: colors.onSurfaceVariant, fontSize: 13 }}>◐</Text>
            <Text
              style={[
                styles.themeName,
                { color: mode === 'system' ? colors.primary : colors.onSurfaceVariant },
                mode === 'system' && styles.themeNameActive,
              ]}
            >
              System (follow device)
            </Text>
          </Pressable>
          {themes.map((t) => {
            const active = mode === 'explicit' && t.id === activeThemeId;
            const preview = JSON.parse(t.colors_json) as Record<string, string>;
            return (
              <Pressable
                key={t.id}
                onPress={() => selectTheme(t.id)}
                hitSlop={4}
                style={[
                  styles.themeOption,
                  { borderColor: active ? colors.primary : colors.outline },
                ]}
              >
                <View style={styles.swatchRow}>
                  <View style={[styles.swatch, { backgroundColor: preview.background }]} />
                  <View style={[styles.swatch, { backgroundColor: preview.surface }]} />
                  <View style={[styles.swatch, { backgroundColor: preview.primary }]} />
                  <View style={[styles.swatch, { backgroundColor: preview.success }]} />
                </View>
                <Text
                  style={[
                    styles.themeName,
                    { color: active ? colors.primary : colors.onSurfaceVariant },
                    active && styles.themeNameActive,
                  ]}
                >
                  {t.name}
                  {t.source === 'plugin' ? '  · plugin' : ''}
                </Text>
              </Pressable>
            );
          })}
        </View>
      ) : null}

      <View style={[styles.errorsHeader, { borderBottomColor: colors.outline }]}>
        <Text style={[styles.sectionTitle, { color: colors.onBackground }]}>🐞 Logged errors</Text>
      </View>

      {loading ? (
        <View style={styles.center}>
          <Text style={[styles.muted, { color: colors.outlineVariant }]}>Loading…</Text>
        </View>
      ) : (
        <FlatList
          data={errors}
          keyExtractor={(item) => item.id}
          ListEmptyComponent={
            <View style={[styles.center, styles.empty]}>
              <Text style={[styles.muted, { color: colors.outlineVariant }]}>
                No errors logged. 🎉
              </Text>
            </View>
          }
          renderItem={({ item }) => (
            <View style={[styles.card, { backgroundColor: colors.surface }]}>
              <View style={styles.cardHeader}>
                <Text style={[styles.context, { color: colors.tertiary }]}>{item.context}</Text>
                <Text style={[styles.time, { color: colors.outlineVariant }]}>
                  {formatTime(item.created_at)}
                </Text>
              </View>
              <Text style={[styles.message, { color: colors.error }]}>{item.message}</Text>
              {item.detail ? (
                <Text numberOfLines={6} style={[styles.detail, { color: colors.onSurfaceVariant }]}>
                  {item.detail}
                </Text>
              ) : null}
            </View>
          )}
          contentContainerStyle={{ paddingBottom: insets.bottom + 24 }}
        />
      )}
    </View>
  );
}

function formatTime(iso: string): string {
  const d = new Date(iso);
  return d.toLocaleString();
}

const styles = StyleSheet.create({
  container: {
    flex: 1,
  },
  header: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  back: {
    fontSize: 15,
  },
  title: {
    fontSize: 18,
    fontWeight: '700',
  },
  clear: {
    fontSize: 15,
  },
  sectionRow: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  sectionTitle: {
    fontSize: 15,
    fontWeight: '600',
  },
  sectionMeta: {
    fontSize: 13,
  },
  themeList: {
    paddingHorizontal: 16,
    paddingVertical: 10,
    gap: 8,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  themeOption: {
    flexDirection: 'row',
    alignItems: 'center',
    gap: 10,
    borderWidth: 1,
    borderRadius: 10,
    paddingHorizontal: 12,
    paddingVertical: 8,
  },
  swatchRow: {
    flexDirection: 'row',
    gap: 3,
  },
  swatch: {
    width: 14,
    height: 14,
    borderRadius: 4,
  },
  themeName: {
    fontSize: 14,
  },
  themeNameActive: {
    fontWeight: '600',
  },
  errorsHeader: {
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  card: {
    borderRadius: 10,
    padding: 12,
    marginHorizontal: 16,
    marginTop: 10,
  },
  cardHeader: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    marginBottom: 4,
  },
  context: {
    fontSize: 12,
    fontWeight: '600',
  },
  time: {
    fontSize: 11,
  },
  message: {
    fontSize: 14,
  },
  detail: {
    fontSize: 11,
    marginTop: 6,
    fontFamily: 'monospace',
  },
  center: {
    flex: 1,
    justifyContent: 'center',
    alignItems: 'center',
  },
  empty: {
    marginTop: 120,
    flex: 0,
  },
  muted: {
    fontSize: 14,
  },
});
