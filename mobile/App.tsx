import { StatusBar } from 'expo-status-bar';
import { Component, useEffect, type ReactNode } from 'react';
import { StyleSheet, Text, View } from 'react-native';
import { GestureHandlerRootView } from 'react-native-gesture-handler';
import { SafeAreaProvider } from 'react-native-safe-area-context';

import { logError } from './src/db/errorsDb';
import { isSyncConfigured, syncNow } from './src/db/turso';
import HomeScreen from './src/screens/HomeScreen';
import { ThemeProvider } from './src/theme/ThemeProvider';

interface BoundaryState {
  message: string | null;
}

/** Catches render-time crashes, logs them, and shows a fallback screen. */
class ErrorBoundary extends Component<{ children: ReactNode }, BoundaryState> {
  state: BoundaryState = { message: null };

  static getDerivedStateFromError(error: unknown): BoundaryState {
    return { message: error instanceof Error ? error.message : String(error) };
  }

  componentDidCatch(error: unknown) {
    logError('render-crash', error);
  }

  render() {
    if (this.state.message) {
      return (
        <View style={styles.crash}>
          <Text style={styles.crashTitle}>Something went wrong</Text>
          <Text style={styles.crashMessage}>{this.state.message}</Text>
          <Text style={styles.crashHint}>The error has been logged.</Text>
        </View>
      );
    }
    return this.props.children;
  }
}

const SYNC_INTERVAL_MS = 5 * 60 * 1000;

/** Periodic background sync — mirrors the TUI's 5-minute cadence. Errors
 * are logged, never surfaced as crashes. */
function SyncRunner() {
  useEffect(() => {
    let cancelled = false;
    const run = async () => {
      try {
        if (await isSyncConfigured()) {
          await syncNow();
        }
      } catch (e) {
        logError('sync', e);
      }
    };
    void run();
    const timer = setInterval(() => {
      if (!cancelled) void run();
    }, SYNC_INTERVAL_MS);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, []);
  return null;
}

export default function App() {
  return (
    <GestureHandlerRootView style={styles.flex}>
      <SafeAreaProvider>
        <ThemeProvider>
          <ErrorBoundary>
            <SyncRunner />
            <HomeScreen />
          </ErrorBoundary>
        </ThemeProvider>
        <StatusBar style="auto" />
      </SafeAreaProvider>
    </GestureHandlerRootView>
  );
}

const styles = StyleSheet.create({
  flex: { flex: 1 },
  crash: {
    flex: 1,
    backgroundColor: '#0f172a',
    justifyContent: 'center',
    padding: 24,
    gap: 8,
  },
  crashTitle: {
    color: '#fecaca',
    fontSize: 20,
    fontWeight: '700',
  },
  crashMessage: {
    color: '#e5e7eb',
    fontSize: 14,
  },
  crashHint: {
    color: '#6b7280',
    fontSize: 12,
  },
});
