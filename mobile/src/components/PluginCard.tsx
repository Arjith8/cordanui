import { StyleSheet, Text, View } from 'react-native';

import type { Widget } from '@/plugin/metadata';
import { useTheme } from '@/theme/ThemeProvider';

/**
 * Declarative card rendered from a plugin's `goals.metadata.mobile` widgets.
 * Vocabulary mirrors `AGENTS.md §12-13` panel widgets so a Lua plugin can
 * target both TUI and mobile with the same JSON.
 *
 * Widget shapes:
 * - { content, fg?, bold? } → text line
 * - { items, highlight? } → list, highlighted row marked with ▸
 * - { children: [...] } → vertical stack
 */

function resolveFg(colors: Record<string, string>, fg?: string): string {
  if (!fg) return colors.onSurface;
  // Known role names resolve to palette entries; unknown names fallback to
  // onSurface (same as TUI's onBackground fallback, but visible on surface).
  const maybe = (colors as Record<string, string>)[fg];
  if (maybe) return maybe;
  // Also try themeColorsOf alias resolution — keep simple.
  return colors.onSurface;
}

function RenderWidget({ widget }: { widget: Widget }) {
  const { colors } = useTheme();

  if ('content' in widget) {
    return (
      <Text
        style={[
          styles.text,
          {
            color: resolveFg(
              colors as unknown as Record<string, string>,
              (widget as { fg?: string }).fg,
            ),
            fontWeight: (widget as { bold?: boolean }).bold ? ('700' as const) : ('400' as const),
          },
        ]}
      >
        {(widget as { content: string }).content}
      </Text>
    );
  }

  if ('items' in widget) {
    const { items, highlight } = widget as { items: string[]; highlight?: number };
    return (
      <View style={styles.list}>
        {items.map((it, idx) => {
          const active = highlight != null && highlight === idx + 1;
          return (
            <View key={`${idx}-${it}`} style={[styles.listRow, active && styles.listRowActive]}>
              <Text
                style={[
                  styles.listGlyph,
                  { color: active ? colors.primary : colors.outlineVariant },
                ]}
              >
                {active ? '▸' : '·'}
              </Text>
              <Text
                style={[
                  styles.listText,
                  { color: active ? colors.primary : colors.onSurface },
                  active && { fontWeight: '600' as const },
                ]}
              >
                {it}
              </Text>
            </View>
          );
        })}
      </View>
    );
  }

  if ('children' in widget) {
    const { children } = widget as { children: Widget[] };
    return (
      <View style={styles.column}>
        {children.map((child, i) => (
          <RenderWidget key={i} widget={child} />
        ))}
      </View>
    );
  }

  return null;
}

export default function PluginCard({ widgets }: { widgets: Widget[] }) {
  const { colors } = useTheme();
  if (!widgets || widgets.length === 0) return null;
  return (
    <View style={[styles.card, { backgroundColor: colors.surface, borderColor: colors.outline }]}>
      {widgets.map((w, i) => (
        <RenderWidget key={i} widget={w} />
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
    gap: 6,
  },
  text: {
    fontSize: 13,
    lineHeight: 18,
  },
  list: {
    gap: 4,
  },
  listRow: {
    flexDirection: 'row',
    alignItems: 'center',
    gap: 8,
    paddingVertical: 2,
  },
  listRowActive: {
    // subtle highlight — no extra bg needed
  },
  listGlyph: {
    fontSize: 12,
    width: 12,
    textAlign: 'center',
  },
  listText: {
    fontSize: 13,
    flex: 1,
  },
  column: {
    gap: 6,
  },
});
