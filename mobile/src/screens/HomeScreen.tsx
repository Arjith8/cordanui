import { useCallback, useEffect, useMemo, useState } from 'react';
import { Alert, Pressable, ScrollView, StyleSheet, Text, View } from 'react-native';
import DraggableFlatList, { type DragEndParams } from 'react-native-draggable-flatlist';
import { useSafeAreaInsets } from 'react-native-safe-area-context';

import { logError } from '@/db/errorsDb';
import {
  createGoal,
  createSheet,
  deleteGoal,
  getAllGoals,
  getSheets,
  initDb,
  updateGoal,
  updateSortOrders,
} from '@/db/goalsDb';
import type { Goal, GoalSheet, GoalStatus } from '@/types/goal';
import { nextStatus } from '@/types/goal';
import { orderGoals } from '@/utils/order';

import ErrorsPage from '@/components/ErrorsPage';
import GoalEditModal from '@/components/GoalEditModal';
import GoalItem from '@/components/GoalItem';
import InlineAddInput from '@/components/InlineAddInput';
import { useTheme } from '@/theme/ThemeProvider';

/**
 * HomeScreen renders goal sheets as tabs. Each sheet shows its goals as a
 * box-drawing accordion tree; new goals are added through inline inputs and
 * always land at the bottom of their sibling list.
 */
export default function HomeScreen() {
  const insets = useSafeAreaInsets();
  const { colors } = useTheme();
  const [sheets, setSheets] = useState<GoalSheet[]>([]);
  const [activeSheetId, setActiveSheetId] = useState<string | null>(null);
  const [addingSheet, setAddingSheet] = useState(false);
  const [sheetNameDraft, setSheetNameDraft] = useState('');

  const [goals, setGoals] = useState<Goal[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [editingGoal, setEditingGoal] = useState<Goal | null>(null);
  const [modalVisible, setModalVisible] = useState(false);
  const [showErrors, setShowErrors] = useState(false);

  const [rootDraft, setRootDraft] = useState('');
  /** The one active subgoal draft. Focusing another input replaces it. */
  const [subgoalDraft, setSubgoalDraft] = useState<{ parentId: string; text: string } | null>(null);

  /** Show the error in the UI and persist it for the errors page. */
  const fail = useCallback((context: string, e: unknown) => {
    setError(e instanceof Error ? e.message : String(e));
    logError(context, e);
  }, []);

  // Bootstrap: db + sheets. The first sheet becomes active.
  useEffect(() => {
    (async () => {
      try {
        setError(null);
        await initDb();
        const all = await getSheets();
        setSheets(all);
        if (all.length > 0) setActiveSheetId(all[0].id);
      } catch (e) {
        fail('init', e);
        setLoading(false);
      }
    })();
  }, [fail]);

  const refresh = useCallback(async () => {
    if (!activeSheetId) return;
    try {
      setError(null);
      setGoals(await getAllGoals(activeSheetId));
    } catch (e) {
      fail('loadGoals', e);
    } finally {
      setLoading(false);
    }
  }, [activeSheetId, fail]);

  useEffect(() => {
    refresh();
  }, [refresh]);

  const byParent = useMemo(() => {
    const map = new Map<string | null, Goal[]>();
    for (const g of goals) {
      const list = map.get(g.parent_id) ?? [];
      list.push(g);
      map.set(g.parent_id, list);
    }
    return map;
  }, [goals]);

  const roots = useMemo(() => orderGoals(byParent.get(null) ?? []), [byParent]);

  const totals = useMemo(() => {
    const total = goals.length;
    let completed = 0;
    for (const g of goals) {
      if (g.status === 'completed') completed++;
    }
    return { total, completed, pending: total - completed };
  }, [goals]);

  const handleSetStatus = useCallback(
    async (id: string, status: GoalStatus) => {
      try {
        await updateGoal(id, {
          status,
          completed_at: status === 'completed' ? new Date().toISOString() : null,
        });
        await refresh();
      } catch (e) {
        fail('setStatus', e);
      }
    },
    [refresh, fail],
  );

  const handleCycleStatus = useCallback(
    async (id: string) => {
      const goal = goals.find((g) => g.id === id);
      if (!goal) return;
      await handleSetStatus(id, nextStatus(goal.status));
    },
    [goals, handleSetStatus],
  );

  const handleRename = useCallback(
    async (id: string, title: string) => {
      try {
        await updateGoal(id, { title });
        await refresh();
      } catch (e) {
        fail('renameGoal', e);
      }
    },
    [refresh, fail],
  );

  const handleSaveDescription = useCallback(
    async (id: string, description: string) => {
      try {
        await updateGoal(id, { description: description.trim() || null });
        await refresh();
      } catch (e) {
        fail('saveDescription', e);
      }
    },
    [refresh, fail],
  );

  /** Persist a sibling group's new visual order as dense sort_orders. */
  const handleReorderGroup = useCallback(
    (group: Goal[]) => {
      void updateSortOrders(group.map((g, idx) => ({ id: g.id, sort_order: idx }))).then(() =>
        refresh(),
      );
    },
    [refresh],
  );

  const handleRootDragEnd = useCallback(
    (params: DragEndParams<Goal>) => handleReorderGroup(params.data),
    [handleReorderGroup],
  );

  const submitRootGoal = useCallback(async () => {
    const title = rootDraft.trim();
    if (!title || !activeSheetId) return;
    try {
      await createGoal({ title, sheet_id: activeSheetId });
      setRootDraft('');
      await refresh();
    } catch (e) {
      fail('addGoal', e);
    }
  }, [rootDraft, activeSheetId, refresh, fail]);

  const draftFor = useCallback(
    (id: string) => (subgoalDraft?.parentId === id ? subgoalDraft.text : ''),
    [subgoalDraft],
  );

  const handleDraftFocus = useCallback((parentId: string) => {
    setSubgoalDraft((prev) => (prev?.parentId === parentId ? prev : { parentId, text: '' }));
  }, []);

  const handleDraftChange = useCallback((parentId: string, text: string) => {
    setSubgoalDraft((prev) => (prev?.parentId === parentId ? { parentId, text } : prev));
  }, []);

  const handleSubmitSubgoal = useCallback(
    async (parentId: string, parentSheetId: string | null) => {
      const title = (subgoalDraft?.parentId === parentId ? subgoalDraft.text : '').trim();
      if (!title) return;
      try {
        await createGoal({ title, parent_id: parentId, sheet_id: parentSheetId });
        // Clear only this input's draft — it was the active one.
        setSubgoalDraft((prev) => (prev?.parentId === parentId ? null : prev));
        await refresh();
      } catch (e) {
        fail('addSubgoal', e);
      }
    },
    [subgoalDraft, refresh, fail],
  );

  const submitSheet = useCallback(async () => {
    const name = sheetNameDraft.trim();
    if (!name) return;
    try {
      const sheet = await createSheet(name);
      setSheets((prev) => [...prev, sheet]);
      setActiveSheetId(sheet.id);
      setSheetNameDraft('');
      setAddingSheet(false);
    } catch (e) {
      fail('createSheet', e);
    }
  }, [sheetNameDraft, fail]);

  const handleDeleteDirect = useCallback(
    (id: string) => {
      Alert.alert('Delete goal', 'This also deletes its subgoals.', [
        { text: 'Cancel', style: 'cancel' },
        {
          text: 'Delete',
          style: 'destructive',
          onPress: () => {
            void deleteGoal(id)
              .then(refresh)
              .catch((e: unknown) => fail('deleteGoal', e));
          },
        },
      ]);
    },
    [refresh, fail],
  );

  const handlePress = useCallback((goal: Goal) => {
    setEditingGoal(goal);
    setModalVisible(true);
  }, []);

  const handleSave = useCallback(
    async (id: string, title: string, description: string) => {
      try {
        await updateGoal(id, { title, description });
        setModalVisible(false);
        await refresh();
      } catch (e) {
        fail('saveGoal', e);
      }
    },
    [refresh, fail],
  );

  const handleDelete = useCallback(
    async (id: string) => {
      try {
        await deleteGoal(id);
        setModalVisible(false);
        await refresh();
      } catch (e) {
        fail('deleteGoal', e);
      }
    },
    [refresh, fail],
  );

  if (loading) {
    return (
      <View style={[styles.container, styles.center, { backgroundColor: colors.background }]}>
        <Text style={[styles.muted, { color: colors.outlineVariant }]}>Loading…</Text>
      </View>
    );
  }

  if (showErrors) {
    return <ErrorsPage onBack={() => setShowErrors(false)} />;
  }

  return (
    <View
      style={[styles.container, { backgroundColor: colors.background, paddingTop: insets.top }]}
    >
      <View style={[styles.headerRow, { borderBottomColor: colors.outline }]}>
        <View style={styles.header}>
          <Text style={[styles.title, { color: colors.onBackground }]}>cordanui</Text>
          <Text style={[styles.subtitle, { color: colors.outlineVariant }]}>
            {totals.completed}/{totals.total} done · {totals.pending} pending
          </Text>
        </View>
        <Pressable
          onPress={() => setShowErrors(true)}
          hitSlop={8}
          style={[styles.profileBtn, { backgroundColor: colors.surface }]}
          accessibilityRole="button"
          accessibilityLabel="Open profile, sync and settings"
        >
          <Text style={[styles.profileIcon, { color: colors.primary }]}>⚙</Text>
        </Pressable>
      </View>

      {error ? (
        <View style={[styles.errorBar, { backgroundColor: colors.error }]}>
          <Text style={[styles.errorText, { color: colors.onPrimary }]}>{error}</Text>
        </View>
      ) : null}

      {/* Sheet tabs */}
      <ScrollView
        horizontal
        showsHorizontalScrollIndicator={false}
        keyboardShouldPersistTaps="handled"
        style={[styles.tabs, { borderBottomColor: colors.outline }]}
        contentContainerStyle={styles.tabsContent}
      >
        {sheets.map((sheet) => (
          <Pressable
            key={sheet.id}
            onPress={() => setActiveSheetId(sheet.id)}
            style={[
              styles.tab,
              { backgroundColor: colors.surface },
              sheet.id === activeSheetId && { backgroundColor: colors.primary },
            ]}
          >
            <Text
              style={[
                styles.tabText,
                { color: colors.onSurfaceVariant },
                sheet.id === activeSheetId && {
                  color: colors.onPrimary,
                  fontWeight: '600' as const,
                },
              ]}
            >
              {sheet.name}
            </Text>
          </Pressable>
        ))}
        {/* Subtle add-sheet affordance */}
        {!addingSheet ? (
          <Pressable
            onPress={() => setAddingSheet(true)}
            hitSlop={8}
            style={[styles.addTab, { borderColor: colors.outlineVariant }]}
          >
            <Text style={[styles.addTabText, { color: colors.onSurfaceVariant }]}>+</Text>
          </Pressable>
        ) : null}
      </ScrollView>

      {/* Sheet naming input — appears focused when adding */}
      {addingSheet ? (
        <InlineAddInput
          value={sheetNameDraft}
          onChangeText={setSheetNameDraft}
          onSubmit={submitSheet}
          placeholder="New sheet name…"
          autoFocus
          onCancel={() => {
            setAddingSheet(false);
            setSheetNameDraft('');
          }}
        />
      ) : null}

      {/* Root-level goal input */}
      <InlineAddInput
        value={rootDraft}
        onChangeText={setRootDraft}
        onSubmit={submitRootGoal}
        placeholder={`Add a goal${sheets.find((s) => s.id === activeSheetId)?.name ? '' : '…'}`}
      />

      <DraggableFlatList
        data={roots}
        keyExtractor={(item) => item.id}
        keyboardShouldPersistTaps="handled"
        containerStyle={{ paddingLeft: 20 }}
        onDragEnd={handleRootDragEnd}
        renderItem={({ item, drag, isActive }) => (
          <GoalItem
            goal={item}
            childrenMap={byParent}
            prefix={[]}
            isLast={roots.indexOf(item) === roots.length - 1}
            dragging={isActive}
            drag={drag}
            onCycleStatus={handleCycleStatus}
            onSetStatus={handleSetStatus}
            onLongPress={handlePress}
            onSubmitSubgoal={handleSubmitSubgoal}
            onRename={handleRename}
            onSaveDescription={handleSaveDescription}
            onDeleteDirect={handleDeleteDirect}
            onReorderGroup={handleReorderGroup}
            draftText={draftFor(item.id)}
            draftFor={draftFor}
            onDraftChange={handleDraftChange}
            onDraftFocus={handleDraftFocus}
          />
        )}
        ListEmptyComponent={
          <View style={styles.empty}>
            <Text style={[styles.muted, { color: colors.outlineVariant }]}>
              This sheet is empty.
            </Text>
            <Text style={[styles.muted, { color: colors.outlineVariant }]}>
              Type above to add your first goal.
            </Text>
          </View>
        }
        contentContainerStyle={{ paddingBottom: insets.bottom + 24, paddingLeft: 20 }}
      />

      <GoalEditModal
        goal={editingGoal}
        visible={modalVisible}
        onClose={() => setModalVisible(false)}
        onSave={handleSave}
        onDelete={handleDelete}
      />
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    flex: 1,
  },
  center: {
    flex: 1,
    justifyContent: 'center',
    alignItems: 'center',
  },
  headerRow: {
    flexDirection: 'row',
    alignItems: 'center',
    justifyContent: 'space-between',
    paddingRight: 12,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  header: {
    paddingHorizontal: 16,
    paddingVertical: 12,
  },
  title: {
    fontSize: 24,
    fontWeight: '700',
  },
  subtitle: {
    fontSize: 13,
    marginTop: 2,
  },
  profileBtn: {
    width: 36,
    height: 36,
    borderRadius: 18,
    justifyContent: 'center',
    alignItems: 'center',
  },
  profileIcon: {
    fontSize: 18,
  },
  tabs: {
    flexGrow: 0,
    borderBottomWidth: StyleSheet.hairlineWidth,
  },
  tabsContent: {
    paddingHorizontal: 12,
    paddingVertical: 8,
    gap: 6,
    alignItems: 'center',
  },
  tab: {
    paddingHorizontal: 14,
    paddingVertical: 6,
    borderRadius: 999,
  },
  tabText: {
    fontSize: 14,
  },
  addTab: {
    width: 28,
    height: 28,
    borderRadius: 999,
    borderWidth: StyleSheet.hairlineWidth,
    justifyContent: 'center',
    alignItems: 'center',
  },
  addTabText: {
    fontSize: 18,
    lineHeight: 20,
    fontWeight: '300',
  },
  empty: {
    alignItems: 'center',
    marginTop: 80,
    gap: 4,
  },
  muted: {
    fontSize: 14,
  },
  errorBar: {
    padding: 8,
    paddingHorizontal: 16,
  },
  errorText: {
    fontSize: 13,
  },
});
