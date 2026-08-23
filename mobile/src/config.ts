/**
 * App configuration — reads Turso sync credentials from env vars.
 *
 * In Expo, env vars are exposed via `process.env` when prefixed with
 * `EXPO_PUBLIC_`. Set these in `.env` or in your environment:
 *
 *   EXPO_PUBLIC_TURSO_URL=libsql://your-db.turso.io
 *   EXPO_PUBLIC_TURSO_TOKEN=your-auth-token
 *   EXPO_PUBLIC_AGENT_URL=http://192.168.1.100:3000
 *   EXPO_PUBLIC_AGENT_TOKEN=your-agent-auth-token
 *
 * If TURSO_URL / TURSO_TOKEN are not set, the app runs in local-only
 * mode (no sync). If AGENT_URL is not set, agent triggers are hidden.
 */

export interface AppConfig {
  tursoUrl: string | null;
  tursoToken: string | null;
  agentUrl: string | null;
  agentToken: string | null;
  syncEnabled: boolean;
  agentEnabled: boolean;
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
    agentEnabled: agentUrl !== null,
  };
}

let cachedConfig: AppConfig | null = null;

export function getConfig(): AppConfig {
  if (!cachedConfig) {
    cachedConfig = loadConfig();
  }
  return cachedConfig;
}
