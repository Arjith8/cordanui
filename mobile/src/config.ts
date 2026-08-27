/**
 * App configuration — reads Turso sync credentials from env vars.
 *
 * In Expo, env vars are exposed via `process.env` when prefixed with
 * `EXPO_PUBLIC_`. Set these in `.env` or in your environment:
 *
 *   EXPO_PUBLIC_TURSO_URL=libsql://your-db.turso.io
 *   EXPO_PUBLIC_TURSO_TOKEN=[redacted]
 *
 * If TURSO_URL / TURSO_TOKEN are not set, the app runs in local-only
 * mode (no sync).
 *
 * Agent capability is discovered at runtime: the TUI writes `agent.url`
 * to the synced settings table when it has an active provider plugin.
 * Mobile reads that setting to show/hide the "assign to agent" UI. An
 * EXPO_PUBLIC_AGENT_URL env var is still supported as a fallback for
 * users who configure the backend directly.
 */

export interface AppConfig {
  tursoUrl: string | null;
  tursoToken: string | null;
  /** Agent backend URL from env var (fallback). The primary source is
   * the synced `agent.url` setting written by the TUI. */
  agentUrl: string | null;
  agentToken: string | null;
  syncEnabled: boolean;
}

export function loadConfig(): AppConfig {
  const tursoUrl = process.env.EXPO_PUBLIC_TURSO_URL ?? null;
  const tursoToken = process.env.EXPO_PUBLIC_TURSO_TOKEN ?? null;
  const agentUrl = process.env.EXPO_PUBLIC_AGENT_URL ?? null;
  const agentToken = process.env.EXPO_PUBLIC_AGENT_TOKEN ?? null;

  return {
    tursoUrl,
    tursoToken,
    agentUrl,
    agentToken,
    syncEnabled: tursoUrl !== null && tursoToken !== null,
  };
}

let cachedConfig: AppConfig | null = null;

export function getConfig(): AppConfig {
  if (!cachedConfig) {
    cachedConfig = loadConfig();
  }
  return cachedConfig;
}
