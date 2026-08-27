import { useCallback, useEffect, useState } from 'react';
import { FlatList, Pressable, StyleSheet, Text, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import { getDb } from '@/db/goalsDb';
import { useTheme } from '@/theme/ThemeProvider';

/**
 * Read-only database viewer: lists every table with its row count, and
 * renders the first 200 rows of the selected table as key/value cards.
 * Reachable from the Profile page. No editing — this is for debugging.
 */
export default function DBViewerPage({ onBack }: { onBack: () => void }) {
  const insets = useSafeAreaInsets();
  const { colors } = useTheme();
  const [tables, setTables] = useState<{ name: string; count: number }[]>([]);
  const [active, setActive] = useState<string | null>(null);
  const [cols, setCols] = useState<string[]>([]);
  const [rows, setRows] = useState<Record<string, unknown>[]>([]);
  const [error, setError] = useState<string | null>(null);

  const loadTables = useCallback(async () => {
    setError(null);
    try {
      const db = await getDb();
      const names = await db.getAllAsync<{ name: string }>(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' ORDER BY name",
      );
      const withCounts: { name: string; count: number }[] = [];
      for (const t of names) {
        const c = await db.getFirstAsync<{ n: number }>(
          `SELECT COUNT(*) AS n FROM "${t.name}"`,
        );
        withCounts.push({ name: t.name, count: c?.n ?? 0 });
      }
      setTables(withCounts);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  const loadTable = useCallback(async (name: string) => {
    setError(null);
    try {
      const db = await getDb();
      const result = await db.getAllAsync<Record<string, unknown>>(
        `SELECT * FROM "${name}" LIMIT 200`,
      );
      setCols(result.length > 0 ? Object.keys(result[0]) : []);
      setRows(result);
      setActive(name);
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, []);

  useEffect(() => {
    loadTables();
  }, [loadTables]);

  const fmt = (v: unknown): string => {
    if (v === null || v === undefined) return '∅';
    if (typeof v === 'object') return JSON.stringify(v);
    return String(v);
  };

  return (
    <View
      style={[styles.container, { backgroundColor: colors.background, paddingTop: insets.top }]}
    >
      <View style={[styles.header, { borderBottomColor: colors.outline }]}>
        <Pressable onPress={active ? () => setActive(null) : onBack} hitSlop={8}>
          <Text style={[styles.back, { color: colors.primary }]}>
            ← {active ? 'Tables' : 'Back'}
          </Text>
        </Pressable>
        <Text style={[styles.title, { color: colors.onBackground }]}>
          {active ?? 'Database'}
        </Text>
        <Pressable onPress={loadTables} hitSlop={8}>
          <Text style={[styles.refresh, { color: colors.primary }]}>↻</Text>
        </Pressable>
      </View>

      {error ? (
        <Text style={[styles.error, { color: colors.error }]}>{error}</Text>
      ) : null}

      {!active ? (
        <FlatList
          data={tables}
          keyExtractor={(t) => t.name}
          contentContainerStyle={{ paddingBottom: insets.bottom + 24 }}
          renderItem={({ item }) => (
            <Pressable
              onPress={() => loadTable(item.name)}
              style={[styles.tableRow, { borderBottomColor: colors.outline }]}
            >
              <Text style={[styles.tableName, { color: colors.onBackground }]}>
                {item.name}
              </Text>
              <Text style={[styles.tableCount, { color: colors.outlineVariant }]}>
                {item.count} rows
              </Text>
            </Pressable>
          )}
          ListEmptyComponent={
            <View style={styles.center}>
              <Text style={[styles.muted, { color: colors.outlineVariant }]}>No tables.</Text>
            </View>
          }
        />
      ) : (
        <FlatList
          data={rows}
          keyExtractor={(_, i) => String(i)}
          contentContainerStyle={{ paddingBottom: insets.bottom + 24 }}
          ListEmptyComponent={
            <View style={styles.center}>
              <Text style={[styles.muted, { color: colors.outlineVariant }]}>
                0 rows (showing up to 200)
              </Text>
            </View>
          }
          renderItem={({ item, index }) => (
            <View style={[styles.card, { backgroundColor: colors.surface }]}>
              <Text style={[styles.rowIndex, { color: colors.outlineVariant }]}>
                #{index + 1}
              </Text>
              {cols.map((c) => (
                <Text key={c} style={[styles.cell, { color: colors.onBackground }]} numberOfLines={4}>
                  <Text style={[styles.cellKey, { color: colors.tertiary }]}>{c}: </Text>
                  {fmt(item[c])}
                </Text>
              ))}
            </View>
          )}
        />
      )}
    </View>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1 },
  header: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  back: { fontSize: 15 },
  title: { fontSize: 16, fontWeight: '600' },
  refresh: { fontSize: 18 },
  error: { paddingHorizontal: 16, paddingVertical: 8, fontSize: 12 },
  tableRow: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  tableName: { fontSize: 14 },
  tableCount: { fontSize: 12 },
  card: {
    marginHorizontal: 16,
    marginTop: 10,
    borderRadius: 8,
    padding: 10,
  },
  rowIndex: { fontSize: 10, marginBottom: 4 },
  cell: { fontSize: 12, marginTop: 2 },
  cellKey: { fontWeight: '600' },
  center: { alignItems: 'center', padding: 24 },
  muted: { fontSize: 13 },
});
