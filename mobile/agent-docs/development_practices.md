# Development practices

> **MANDATORY:** Every agent and every session working in this repository
> must read this file before making any changes. Do not skip it.

## Package manager: pnpm only

This project uses **pnpm**. Using npm or yarn anywhere — installs, scripts,
or invoking binaries — is a mistake.

- Install: `pnpm install`
- Add a dependency: `pnpm add <pkg>` / dev: `pnpm add -D <pkg>`
- Run project scripts: `pnpm lint`, `pnpm format`, `pnpm typecheck`
- Invoke local binaries / Expo CLI: `pnpm exec <cmd>` (e.g. `pnpm exec expo start`)
- The pinned version lives in `packageManager` in `package.json`. Don't change
  it casually.

## Quality gates

Before finishing any change, all of these must pass:

1. `pnpm typecheck`
2. `pnpm lint`

Formatting is handled by Biome (`pnpm format`). Keep formatting changes out of
unrelated commits.

## Conventions

- TypeScript strict mode is on. Do not weaken it or use `any` to silence errors.
- Path alias: `@/*` maps to `src/*`. Use it for imports.
- Only code inside `src/db/` may talk to storage drivers directly. Screens and
  components consume the public API of that layer.
- Types in `src/types/goal.ts` mirror the shared SQL schema
  (`../rust/schema/schema.sql`). The schema is the source of truth — update the
  types whenever the schema changes.
- Env vars are read via `EXPO_PUBLIC_*` prefixes through `src/config.ts`.
  Don't read `process.env` directly elsewhere.

## Upgrading Expo

Use pnpm end-to-end:

```bash
pnpm add expo@latest
pnpm exec expo install --fix
pnpm typecheck && pnpm lint
```

Check the Expo SDK changelog for breaking changes before upgrading across
multiple SDK versions.
