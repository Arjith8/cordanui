import {
  type ReactNode,
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from 'react';
import { useColorScheme } from 'react-native';

import { logError } from '@/db/errorsDb';
import { getThemeState, listThemes, selectTheme as persistSelection } from '@/db/themeDb';
import type { ThemeColors, ThemeMode, ThemeRecord } from '@/theme/types';
import { FALLBACK_COLORS, themeColorsOf } from '@/theme/types';

interface ThemeContextValue {
  /** Resolved token map — the only colors any component should use. */
  colors: ThemeColors;
  mode: ThemeMode;
  scheme: 'light' | 'dark';
  themes: ThemeRecord[];
  activeThemeId: string;
  /** null → back to system mode. */
  selectTheme: (id: string | null) => Promise<void>;
  refreshThemes: () => Promise<void>;
}

const ThemeContext = createContext<ThemeContextValue | null>(null);

export function ThemeProvider({ children }: { children: ReactNode }) {
  const osScheme = useColorScheme();
  const scheme: 'light' | 'dark' = osScheme === 'light' ? 'light' : 'dark';

  const [colors, setColors] = useState<ThemeColors>(FALLBACK_COLORS);
  const [mode, setMode] = useState<ThemeMode>('system');
  const [themes, setThemes] = useState<ThemeRecord[]>([]);
  const [activeThemeId, setActiveThemeId] = useState('builtin-dark');

  const reload = useCallback(async () => {
    try {
      const state = await getThemeState(scheme);
      setColors(themeColorsOf(state.active));
      setActiveThemeId(state.active.id);
      setMode(state.mode);
      setThemes(state.themes);
    } catch (e) {
      logError('theme.load', e);
    }
  }, [scheme]);

  useEffect(() => {
    reload();
  }, [reload]);

  const selectTheme = useCallback(
    async (id: string | null) => {
      try {
        await persistSelection(id);
        await reload();
      } catch (e) {
        logError('theme.select', e);
      }
    },
    [reload],
  );

  const refreshThemes = useCallback(async () => {
    try {
      setThemes(await listThemes());
    } catch (e) {
      logError('theme.refresh', e);
    }
  }, []);

  const value = useMemo(
    () => ({ colors, mode, scheme, themes, activeThemeId, selectTheme, refreshThemes }),
    [colors, mode, scheme, themes, activeThemeId, selectTheme, refreshThemes],
  );

  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>;
}

export function useTheme(): ThemeContextValue {
  const ctx = useContext(ThemeContext);
  if (!ctx) throw new Error('useTheme must be used inside <ThemeProvider>');
  return ctx;
}
