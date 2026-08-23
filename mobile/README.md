# cordanui — mobile

React Native (Expo) client for cordanui.

## status

Phase 1 (local-first scaffold). Reads/writes a local SQLite DB that
mirrors the shared schema (`../rust/schema/schema.sql`). No Turso sync yet,
no plugin system, no agent triggers — just goal CRUD with nested
subgoals.

## run

```bash
pnpm install
npx expo start
```

Then scan the QR with Expo Go (Android) or the Camera app (iOS), or press
`a` / `i` to launch on an emulator.

## structure

```
src/
├── types/goal.ts        # shared types, mirrors rust/schema/schema.sql
├── db/goalsDb.ts        # local SQLite layer (expo-sqlite). Swaps to libSQL in phase 2.
├── components/
│   ├── GoalRow.tsx         # a single goal row in the tree
│   ├── StatusCircle.tsx    # ascii status circle: pending / wip / done
│   └── GoalEditModal.tsx   # edit / delete sheet
└── screens/
    └── HomeScreen.tsx      # goal sheets (tabs) — tree view, inline add inputs
```

## data layer

The DB API in `src/db/goalsDb.ts` is the only module that touches storage.
When phase 2 (Turso sync) lands, its internals swap to libSQL — the public
API stays the same. Screens and components don't import the driver
directly.
