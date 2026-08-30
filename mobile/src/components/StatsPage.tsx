import { useMemo } from 'react';
import { Pressable, ScrollView, StyleSheet, Text, View } from 'react-native';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import type { Goal, GoalSheet } from '@/types/goal';
import { statusColor } from '@/theme/types';
import { useTheme } from '@/theme/ThemeProvider';

type Props = {
  goals: Goal[];
  sheets: GoalSheet[];
  onBack: () => void;
};

export default function StatsPage({ goals, sheets, onBack }: Props) {
  const insets = useSafeAreaInsets();
  const { colors } = useTheme();

  const stats = useMemo(() => {
    const total = goals.length;
    const byStatus = {
      pending: 0,
      in_progress: 0,
      completed: 0,
      agent_mode: 0,
    } as Record<string, number>;
    for (const g of goals) {
      byStatus[g.status] = (byStatus[g.status] ?? 0) + 1;
    }

    const now = new Date();
    const todayStart = new Date(now.getFullYear(), now.getMonth(), now.getDate());
    const weekEnd = new Date(todayStart);
    weekEnd.setDate(weekEnd.getDate() + 7);

    let overdue = 0;
    let dueToday = 0;
    let dueWeek = 0;
    let noDue = 0;
    let remindCount = 0;

    const repeatCounts: Record<string, number> = {
      none: 0,
      daily: 0,
      weekly: 0,
      monthly: 0,
      yearly: 0,
    };

    let childCount = 0;
    const parentIds = new Set<string>();
    for (const g of goals) {
      if (g.parent_id) childCount++;
      if (g.parent_id) parentIds.add(g.parent_id);
    }

    for (const g of goals) {
      if (!g.due_at) {
        noDue++;
      } else {
        const due = new Date(g.due_at);
        if (due < now && g.status !== 'completed') overdue++;
        const dueDay = new Date(due.getFullYear(), due.getMonth(), due.getDate());
        if (dueDay.getTime() === todayStart.getTime()) dueToday++;
        if (due >= todayStart && due < weekEnd) dueWeek++;
      }
      if (g.remind_at) remindCount++;
      const rule = (g.repeat_rule ?? '').toLowerCase().trim();
      if (!rule) repeatCounts.none++;
      else if (rule.includes('daily')) repeatCounts.daily++;
      else if (rule.includes('weekly')) repeatCounts.weekly++;
      else if (rule.includes('monthly')) repeatCounts.monthly++;
      else if (rule.includes('yearly') || rule.includes('annually') || rule.includes('annual')) repeatCounts.yearly++;
      else repeatCounts.none++;
    }

    // sheets distribution
    const sheetCounts: { label: string; count: number }[] = [];
    const bySheet = new Map<string, number>();
    let unsheeted = 0;
    for (const g of goals) {
      if (!g.sheet_id) unsheeted++;
      else bySheet.set(g.sheet_id, (bySheet.get(g.sheet_id) ?? 0) + 1);
    }
    for (const s of sheets) {
      sheetCounts.push({ label: s.name, count: bySheet.get(s.id) ?? 0 });
    }
    if (unsheeted > 0) sheetCounts.push({ label: 'Unsheeted', count: unsheeted });

    const parentCount = parentIds.size;
    const avgChildren = parentCount > 0 ? childCount / parentCount : 0;
    const completionRate = total > 0 ? (byStatus.completed / total) * 100 : 0;

    return {
      total,
      byStatus,
      overdue,
      dueToday,
      dueWeek,
      noDue,
      remindCount,
      repeatCounts,
      sheetCounts,
      avgChildren,
      completionRate,
      childCount,
    };
  }, [goals, sheets]);

  const bar = (count: number, total: number, color: string) => {
    const pct = total > 0 ? Math.round((count / total) * 100) : 0;
    return { pct, color };
  };

  if (stats.total === 0) {
    return (
      <View style={[styles.container, { backgroundColor: colors.background, paddingTop: insets.top }]}>
        <View style={[styles.header, { borderBottomColor: colors.outline }]}>
          <Pressable onPress={onBack} hitSlop={8}>
            <Text style={[styles.back, { color: colors.primary }]}>← Back</Text>
          </Pressable>
          <Text style={[styles.title, { color: colors.onBackground }]}>Stats</Text>
          <View style={{ width: 48 }} />
        </View>
        <View style={styles.center}>
          <Text style={[styles.muted, { color: colors.outlineVariant }]}>No data yet.</Text>
        </View>
      </View>
    );
  }

  const statusEntries: { label: string; key: string; color: string }[] = [
    { label: 'Pending', key: 'pending', color: statusColor(colors, 'pending') },
    { label: 'In progress', key: 'in_progress', color: statusColor(colors, 'in_progress') },
    { label: 'Completed', key: 'completed', color: statusColor(colors, 'completed') },
    { label: 'Agent', key: 'agent_mode', color: statusColor(colors, 'agent_mode') },
  ];

  return (
    <View style={[styles.container, { backgroundColor: colors.background, paddingTop: insets.top }]}>
      <View style={[styles.header, { borderBottomColor: colors.outline }]}>
        <Pressable onPress={onBack} hitSlop={8}>
          <Text style={[styles.back, { color: colors.primary }]}>← Back</Text>
        </Pressable>
        <Text style={[styles.title, { color: colors.onBackground }]}>Stats</Text>
        <View style={{ width: 48 }} />
      </View>

      <ScrollView contentContainerStyle={{ paddingBottom: insets.bottom + 24 }}>
        {/* Overview */}
        <View style={[styles.sectionRow, { borderBottomColor: colors.outline }]}>
          <Text style={[styles.sectionTitle, { color: colors.onBackground }]}>Overview</Text>
        </View>
        <View style={styles.cardWrap}>
          <View style={[styles.card, { backgroundColor: colors.surface }]}>
            <View style={styles.kvRow}>
              <Text style={[styles.kvLabel, { color: colors.onSurfaceVariant }]}>Total goals</Text>
              <Text style={[styles.kvValue, { color: colors.onSurface }]}>{stats.total}</Text>
            </View>
            <View style={styles.kvRow}>
              <Text style={[styles.kvLabel, { color: colors.onSurfaceVariant }]}>Completion</Text>
              <Text style={[styles.kvValue, { color: colors.success }]}>{stats.completionRate.toFixed(0)}%</Text>
            </View>
            <View style={styles.kvRow}>
              <Text style={[styles.kvLabel, { color: colors.onSurfaceVariant }]}>Avg children / parent</Text>
              <Text style={[styles.kvValue, { color: colors.onSurface }]}>{stats.avgChildren.toFixed(1)}</Text>
            </View>
          </View>
        </View>

        {/* Status */}
        <View style={[styles.sectionRow, { borderBottomColor: colors.outline }]}>
          <Text style={[styles.sectionTitle, { color: colors.onBackground }]}>Status</Text>
        </View>
        <View style={styles.cardWrap}>
          <View style={[styles.card, { backgroundColor: colors.surface }]}>
            {statusEntries.map((e) => {
              const count = stats.byStatus[e.key] ?? 0;
              const { pct } = bar(count, stats.total, e.color);
              return (
                <View key={e.key} style={styles.barRow}>
                  <Text style={[styles.barLabel, { color: colors.onSurfaceVariant }]}>{e.label}</Text>
                  <View style={[styles.barTrack, { backgroundColor: colors.surfaceVariant }]}>
                    <View style={[styles.barFill, { width: `${pct}%`, backgroundColor: e.color }]} />
                  </View>
                  <Text style={[styles.barValue, { color: colors.onSurface }]}>{count} · {pct}%</Text>
                </View>
              );
            })}
          </View>
        </View>

        {/* Due / Reminders */}
        <View style={[styles.sectionRow, { borderBottomColor: colors.outline }]}>
          <Text style={[styles.sectionTitle, { color: colors.onBackground }]}>Due & Reminders</Text>
        </View>
        <View style={styles.cardWrap}>
          <View style={[styles.card, { backgroundColor: colors.surface }]}>
            <Row label="Overdue" value={String(stats.overdue)} colors={colors} highlight={stats.overdue > 0 ? colors.error : undefined} />
            <Row label="Due today" value={String(stats.dueToday)} colors={colors} />
            <Row label="Due within 7d" value={String(stats.dueWeek)} colors={colors} />
            <Row label="No due date" value={String(stats.noDue)} colors={colors} />
            <Row label="Reminders set" value={String(stats.remindCount)} colors={colors} />
          </View>
        </View>

        {/* Repeat */}
        <View style={[styles.sectionRow, { borderBottomColor: colors.outline }]}>
          <Text style={[styles.sectionTitle, { color: colors.onBackground }]}>Repeat</Text>
        </View>
        <View style={styles.cardWrap}>
          <View style={[styles.card, { backgroundColor: colors.surface }]}>
            {Object.entries(stats.repeatCounts).map(([k, v]) => (
              <Row key={k} label={k} value={String(v)} colors={colors} />
            ))}
          </View>
        </View>

        {/* Sheets */}
        <View style={[styles.sectionRow, { borderBottomColor: colors.outline }]}>
          <Text style={[styles.sectionTitle, { color: colors.onBackground }]}>Sheets</Text>
        </View>
        <View style={styles.cardWrap}>
          <View style={[styles.card, { backgroundColor: colors.surface }]}>
            {stats.sheetCounts.map((s) => (
              <Row key={s.label} label={s.label} value={String(s.count)} colors={colors} />
            ))}
          </View>
        </View>

        {/* Agent */}
        <View style={[styles.sectionRow, { borderBottomColor: colors.outline }]}>
          <Text style={[styles.sectionTitle, { color: colors.onBackground }]}>Agent</Text>
        </View>
        <View style={styles.cardWrap}>
          <View style={[styles.card, { backgroundColor: colors.surface }]}>
            <Row label="Agent mode" value={String(stats.byStatus.agent_mode ?? 0)} colors={colors} />
          </View>
        </View>
      </ScrollView>
    </View>
  );
}

function Row({
  label,
  value,
  colors,
  highlight,
}: {
  label: string;
  value: string;
  colors: { onSurfaceVariant: string; onSurface: string };
  highlight?: string;
}) {
  return (
    <View style={styles.kvRow}>
      <Text style={[styles.kvLabel, { color: colors.onSurfaceVariant }]}>{label}</Text>
      <Text style={[styles.kvValue, { color: highlight ?? colors.onSurface }]}>{value}</Text>
    </View>
  );
}

const styles = StyleSheet.create({
  container: { flex: 1 },
  center: { flex: 1, justifyContent: 'center', alignItems: 'center' },
  muted: { fontSize: 14 },
  header: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  back: { fontSize: 15 },
  title: { fontSize: 18, fontWeight: '700' },
  sectionRow: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    paddingHorizontal: 16,
    paddingVertical: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  sectionTitle: { fontSize: 15, fontWeight: '600' },
  cardWrap: { paddingHorizontal: 16, paddingTop: 10 },
  card: {
    borderRadius: 10,
    padding: 12,
    gap: 8,
  },
  kvRow: {
    flexDirection: 'row',
    justifyContent: 'space-between',
    alignItems: 'center',
  },
  kvLabel: { fontSize: 13 },
  kvValue: { fontSize: 14, fontWeight: '600' },
  barRow: {
    flexDirection: 'row',
    alignItems: 'center',
    gap: 8,
  },
  barLabel: { fontSize: 13, width: 84 },
  barTrack: {
    flex: 1,
    height: 8,
    borderRadius: 4,
    overflow: 'hidden',
  },
  barFill: {
    height: '100%',
    borderRadius: 4,
  },
  barValue: { fontSize: 12, width: 64, textAlign: 'right' },
});
