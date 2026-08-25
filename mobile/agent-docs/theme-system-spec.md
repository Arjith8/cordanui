# Theme system spec

> Reference for agents implementing or extending the "styles from DB" system
> in cordanui/mobile. Read alongside `agent-docs/development_practices.md`
> (mandatory) — this document is the deep-dive on theming only.

## Goal

All UI colors come from a **token map stored in SQLite**, resolved at runtime.
Users (and later, plugins installed via the TUI) can define themes; the app
applies the most recently used one, defaulting to following the OS light/dark
scheme.

## Data model (local SQLite, migration v2 `theme-system`)

Two tables, created in `src/db/goalsDb.ts` (`SCHEMA_SQL`) and seeded by
migration v2:

### `themes`

| column       | type    | notes                                             |
|--------------|---------|---------------------------------------------------|
| id           | TEXT PK | builtin ids: `builtin-dark`, `builtin-light`; plugin themes use UUIDs |
| name         | TEXT    | display name                                       |
| source       | TEXT    | `'builtin'` \| `'plugin'`                          |
| is_dark      | INT     | 0/1 — used for system-mode resolution              |
| colors_json  | TEXT    | JSON object: partial token map (see below)         |
| last_used_at | TEXT    | ISO timestamp; NULL until explicitly selected      |

### `settings` (generic KV)

| key                | value meaning                                  |
|--------------------|------------------------------------------------|
| `theme_mode`       | `'system'` (default) \| `'explicit'`           |
| `selected_theme_id`| themes.id when mode is `'explicit'`            |

## Tokens (the styling constants)

Defined once in `src/theme/types.ts` as `ThemeColors`. Components must never
hardcode hex values; they consume tokens via `useTheme().colors`.

> **Both hosts now speak the role vocabulary (2026-08):** colors are
> addressed by **Compose / Material 3 role names** — `background`,
> `onBackground`, `surface`, `onSurface`, `surfaceVariant`,
> `onSurfaceVariant`, `primary`, `onPrimary`, `secondary`, `onSecondary`,
> `tertiary`, `onTertiary`, `success`, `onSuccess`, `error`, `onError`,
> `outline`, `outlineVariant`. Widget-specific tokens (`statusWip` etc.)
> are gone everywhere; statuses use standard roles (pending →
> `onSurfaceVariant`, wip → `primary`, done → `success`, agent →
> `tertiary`) via `statusColor()`.
>
> **Legacy rows still render:** `themeColorsOf()` aliases old token names
> (`bg` → `background`, `accent` → `primary`, `statusDone` → `success`,
> ...) before applying canonical keys, so plugin themes written against
> the old vocabulary keep working. New themes should use canonical roles;
> dual-keying is no longer necessary but remains harmless.
>
> **Synced user overrides:** the TUI's `cord.g.style.*` persists
> per-variable overrides as `settings` rows keyed `style.<var>`. Mobile
> reads them in `getThemeState()` and applies them **above** the active
> theme (`themeColorsOf(active, styleOverrides)`). They arrive via the
> Turso sync (`src/db/turso.ts` — settings are pull-only, remote wins;
> runs on app start, every 5 minutes, and via "Sync now" on the Profile
> page), so restyling the TUI restyles this app within a few minutes or
> on next sync. Credentials live in local-only `settings` keys
> (`turso_url` / `turso_token`) that are excluded from pulls and never
> pushed.

Token table (`ThemeColors` in `src/theme/types.ts`):

| role               | semantic                                    |
|--------------------|---------------------------------------------|
| `background`       | screen background                           |
| `onBackground`     | primary text                                |
| `surface`          | cards, inputs, inactive tabs                |
| `onSurface`        | text/icons on surfaces                      |
| `surfaceVariant`   | muted surface variant                       |
| `onSurfaceVariant` | secondary text                              |
| `primary`          | primary action color; ◐ wip status          |
| `onPrimary`        | text/icons on top of primary                |
| `secondary`        | secondary accent                            |
| `onSecondary`      | text/icons on top of secondary              |
| `tertiary`         | third accent; ⤴ agent status                |
| `onTertiary`       | text/icons on top of tertiary               |
| `success`          | ● done status / success states              |
| `onSuccess`        | text/icons on top of success                |
| `error`            | destructive actions / error text            |
| `onError`          | text/icons on top of error                  |
| `outline`          | hairline borders and dividers               |
| `outlineVariant`   │ tree guides, placeholders, faint text      |

Legacy alias map (read-only compatibility, see `LEGACY_ALIASES`):
`bg`→`background`, `border`→`outline`, `treeLine`→`outlineVariant`,
`text`→`onBackground`, `textDim`→`onSurfaceVariant`,
`textFaint`→`outlineVariant`, `accent`→`primary`, `onAccent`→`onPrimary`,
`danger`→`error`, `statusPending`→`onSurfaceVariant`,
`statusWip`→`primary`, `statusDone`→`success`, `statusAgent`→`tertiary`.

`colors_json` may specify **any subset** of tokens. Missing keys are filled
from `DARK_THEME_COLORS` by `themeColorsOf()` (`{ ...DARK_THEME_COLORS,
...JSON.parse(colors_json) }`). Values must be hex strings RN accepts
(`#rrggbb`; alpha suffixes like `${color}80` are appended at usage sites).
The TUI additionally accepts `#rgb`, `rgb(r,g,b)` and `rgba(...)` forms in
the same column — tolerate unknown formats by falling back to defaults.

Example plugin theme (carrying both vocabularies so one row renders
correctly on clients that haven't adopted the aliasing yet):

```json
{
  "id": "uuid-from-plugin-system",
  "name": "Nord",
  "source": "plugin",
  "isDark": true,
  "colorsJson": "{\"background\":\"#2e3440\",\"primary\":\"#88c0d0\",\"onBackground\":\"#eceff4\",\"bg\":\"#2e3440\",\"accent\":\"#88c0d0\",\"text\":\"#eceff4\"}"
}
```

## Resolution flow

1. On boot, `ThemeProvider` (`src/theme/ThemeProvider.tsx`) reads OS scheme
   via `useColorScheme()`.
2. Calls `getThemeState(scheme)` (`src/db/themeDb.ts`):
   - mode `'explicit'` → row matching `selected_theme_id`;
   - otherwise → builtin row where `is_dark` matches the OS scheme.
   - Falls back to a builtin if the explicit selection no longer exists.
3. Resolved record → `themeColorsOf()` → context state.
4. **Reactive:** whenever the OS scheme changes while mode is `'system'`,
   resolution re-runs automatically.
5. Until the DB resolves (a few ms), context serves `FALLBACK_COLORS` (dark)
   so first paint is styled, not blank. There is deliberately **no**
   AsyncStorage cache — SQLite open is fast enough and we avoid another
   storage dependency.

Selection writes go through `selectTheme(id | null)`:
- `null` → mode back to `'system'` (OS-following);
- an id → mode `'explicit'`, sets `selected_theme_id`, stamps
  `last_used_at`.

Theme lists everywhere are ordered **most-recently-used first**:
`ORDER BY last_used_at IS NULL, last_used_at DESC, name`.

## Plugin contract (for the TUI-installed plugin flow)

Plugins do not touch mobile code. They register themes through:

```ts
upsertTheme({ name, source: 'plugin', isDark, colorsJson }) // → returns id
```

(INSERT … ON CONFLICT UPDATE on `themes.id`). After install, the theme simply
appears in the picker (`Profile → 🎨 Themes`). Unknown/missing tokens fall
back to dark defaults per `themeColorsOf()`, so a plugin shipping 3 tokens
still renders sanely.

## Component rules (enforced by convention)

1. Import `useTheme()` from `@/theme/ThemeProvider`; read `const { colors }`.
2. Layout stays in static `StyleSheet.create`; **colors are applied inline**:
   `style={[styles.card, { backgroundColor: colors.surface }]}`.
3. Status colors: never map glyphs to hex locally — use
   `statusColor(colors, status)` from `src/theme/types.ts`.
4. Adding a token = add to `ThemeColors` + both builtin palettes +
   this doc. TypeScript will flag every missing usage site.

## File map

| file                        | responsibility                                  |
|-----------------------------|--------------------------------------------------|
| `src/theme/types.ts`        | tokens, palettes, `ThemeRecord`, helpers         |
| `src/db/goalsDb.ts`         | table DDL + migration v2 seeding builtins        |
| `src/db/themeDb.ts`         | all theme SQL: get/list/select/upsert            |
| `src/theme/ThemeProvider.tsx`| React context, resolution, `useTheme()`          |
| `src/components/ErrorsPage.tsx` | Profile page incl. 🎨 Themes picker           |

## Migration discipline

The tables ship in migration **v2** of the versioned runner in
`goalsDb.ts` (`LATEST_SCHEMA_VERSION`, `schema_migrations` table); the
token-role re-seed of the builtins is **v3** (`theme-token-roles`). Any new
theming columns/tables must be a **new numbered migration step**, never an
edit to existing steps. See `agent-docs/development_practices.md`.
