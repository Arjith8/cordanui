import { Pressable, StyleSheet, Text } from 'react-native';

import { useTheme } from '@/theme/ThemeProvider';
import { statusColor } from '@/theme/types';
import type { GoalStatus } from '@/types/goal';

/**
 * ASCII status circle. Three visual states the user cycles through by
 * tapping: ○ pending → ◐ wip → ● done. Colors come from the active theme.
 */
const GLYPHS: Record<GoalStatus, string> = {
  pending: '○',
  in_progress: '◐',
  completed: '●',
  agent_mode: '⤴', // set programmatically; not part of the tap cycle
};

export interface StatusCircleProps {
  status: GoalStatus;
  onPress?: () => void;
}

export default function StatusCircle({ status, onPress }: StatusCircleProps) {
  const { colors } = useTheme();
  return (
    <Pressable onPress={onPress} hitSlop={10} style={styles.circle}>
      <Text style={[styles.glyph, { color: statusColor(colors, status) }]}>{GLYPHS[status]}</Text>
    </Pressable>
  );
}

const styles = StyleSheet.create({
  circle: {
    width: 32,
    height: 32,
    justifyContent: 'center',
    alignItems: 'center',
  },
  glyph: {
    fontSize: 22,
    lineHeight: 26,
    textAlign: 'center',
  },
});
