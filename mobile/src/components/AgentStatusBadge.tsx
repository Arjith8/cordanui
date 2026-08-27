import { Pressable, StyleSheet, Text, View } from 'react-native';

import { useTheme } from '@/theme/ThemeProvider';
import type { Goal } from '@/types/goal';
import { agentStatusLabel, parseAgentProgress, parseAgentResult } from '@/db/agentDb';

/**
 * Agent status badge — shown on goals in `agent_mode`. Displays the
 * agent's current status (queued / running / completed / failed) and,
 * when expanded, the result content or last progress message.
 *
 * This component only renders when `goal.status === 'agent_mode'`.
 * The parent decides whether to show it based on whether agent
 * capability is available (see `isAgentAvailable` in agentDb).
 */
export interface AgentStatusBadgeProps {
  goal: Goal;
  /** When true, the badge is expanded to show result/progress detail. */
  expanded?: boolean;
  /** Called when the user taps the badge to expand/collapse. */
  onToggle?: () => void;
}

export default function AgentStatusBadge({
  goal,
  expanded = false,
  onToggle,
}: AgentStatusBadgeProps) {
  const { colors } = useTheme();

  if (goal.status !== 'agent_mode') return null;

  const status = goal.agent_status;
  const label = agentStatusLabel(status);
  const result = parseAgentResult(goal);
  const progress = parseAgentProgress(goal);

  // Color by agent status: queued/running → tertiary, completed → success,
  // failed → error.
  const badgeColor =
    status === 'completed'
      ? colors.success
      : status === 'failed'
        ? colors.error
        : colors.tertiary;

  return (
    <View style={styles.container}>
      <Pressable
        onPress={onToggle}
        hitSlop={6}
        style={[styles.badge, { backgroundColor: `${badgeColor}20`, borderColor: badgeColor }]}
      >
        <Text style={[styles.badgeText, { color: badgeColor }]}>⤴ {label}</Text>
      </Pressable>

      {expanded ? (
        <View style={[styles.detail, { backgroundColor: colors.surfaceVariant }]}>
          {status === 'failed' && result ? (
            <Text style={[styles.resultText, { color: colors.error }]}>
              {result.content}
            </Text>
          ) : status === 'completed' && result ? (
            <Text style={[styles.resultText, { color: colors.onSurface }]}>
              {result.content}
            </Text>
          ) : status === 'running' && progress ? (
            <Text style={[styles.progressText, { color: colors.onSurfaceVariant }]}>
              {progress.message}
              {progress.detail ? `\n${progress.detail}` : ''}
            </Text>
          ) : status === 'queued' ? (
            <Text style={[styles.progressText, { color: colors.onSurfaceVariant }]}>
              Waiting for agent backend to pick up this task…
            </Text>
          ) : null}
        </View>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  container: {
    marginTop: 6,
  },
  badge: {
    alignSelf: 'flex-start',
    flexDirection: 'row',
    alignItems: 'center',
    gap: 4,
    borderWidth: 1,
    borderRadius: 999,
    paddingHorizontal: 10,
    paddingVertical: 3,
  },
  badgeText: {
    fontSize: 12,
    fontWeight: '500',
  },
  detail: {
    marginTop: 6,
    borderRadius: 8,
    padding: 12,
  },
  resultText: {
    fontSize: 13,
    lineHeight: 18,
  },
  progressText: {
    fontSize: 13,
    lineHeight: 18,
    fontStyle: 'italic',
  },
});
