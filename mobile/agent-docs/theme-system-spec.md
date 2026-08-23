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

| token          | semantic                                    |
|----------------|---------------------------------------------|
| `bg`           | screen background                           |
| `surface`      | cards, inputs, inactive tabs                 |
| `border`       | hairline borders/dividers                    |
| `treeLine`     | tree guide/elbow lines                       |
| `text`         | primary text                                 |
| `textDim`      | secondary text                               |
| `textFaint`    | placeholders, hints, de-emphasized text      |
| `accent`       | primary action color                         |
| `onAccent`     | text/icon color on top of accent             |
| `danger`       | destructive actions / error text             |
| `statusPending`| ○ pending status                             |
| `statusWip`    | ◐ wip status                                 |
| `statusDone`   | ● done status                                |
| `statusAgent`  | agent-mode status + profile icon             |

`colors_json` may specify **any subset** of tokens. Missing keys are filled
from `DARK_THEME_COLORS` by `themeColorsOf()` (`{ ...DARK_THEME_COLORS,
...JSON.parse(colors_json) }`). Values must be hex strings RN accepts
(`#rrggbb`; alpha suffixes like `${color}80` are appended at usage sites).

Example plugin theme:

```json
{
  "id": "uuid-from-plugin-system",
  "name": "Nord",
  "source": "plugin",
  "isDark": true,
  "colorsJson": "{\"bg\":\"#2e3440\",\"surface\":\"#3b4252\",\"accent\":\"#88c0d0\",\"text\":\"#eceff4\"}"
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
`goalsDb.ts` (`LATEST_SCHEMA_VERSION`, `schema_migrations` table). Any new
theming columns/tables must be a **new numbered migration step**, never an
edit to existing steps. See `agent-docs/development_practices.md`.
