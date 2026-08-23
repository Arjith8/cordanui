# Agent instructions

## Mandatory reading

**Every agent and every session MUST read `agent-docs/development_practices.md`
before doing any work in this repository.** No exceptions.

Reading `agent-docs/project_overview.md` is optional but recommended if you
need context on what this app is. Working on theming or plugin-provided
themes? See `agent-docs/theme-system-spec.md`.

## Package manager

This project uses **pnpm** exclusively. Never use npm or yarn — not for
installing dependencies, not for running scripts, not for npx-style
invocations of project tooling (`pnpm exec` instead).

```bash
pnpm install
pnpm lint        # biome check .
pnpm format      # biome format --write .
pnpm typecheck   # tsc --noEmit
pnpm exec expo start
```
