# Sync divergence experiment

> Empirical test of what actually happens when an embedded replica (TUI)
> and the remote primary (mobile's view) diverge. Run this before trusting
> any of the LWW assumptions in the codebase.

## Run

```sh
cd rust
cargo run -p cordanui-sync --example divergence -- \
  libsql://your-db.turso.io your-auth-token
```

Safe to run repeatedly — each scenario uses fresh goal ids (`s1-*` …
`s5-*`). Uses the same protocol split as production: an **embedded
replica** plays the TUI, a **libSQL remote client** (Hrana over HTTP)
plays mobile.

## Scenarios

| # | Scenario | The question |
|---|----------|--------------|
| S1 | Replica adds A offline; remote adds C; replica syncs | Do additive changes from both sides survive? |
| S2 | Both edit goal B; remote's `updated_at` is newer | Does the newer (mobile) edit win? Does sync error? |
| S3 | Both edit goal B; replica's `updated_at` is newer | Does the newer (TUI) edit win — or does remote win unconditionally? |
| S4 | Replica deletes D offline; syncs | Does the delete propagate, or does D resurrect locally/remotely? |
| S5 | Two replicas edit the same row, sync in sequence | What does the second sync do to the first's write? |

## What to record per scenario

- Did `sync()` return ok or an error (paste the error)?
- Replica state vs remote state after sync (the harness prints both)
- Which side's write won, and whether anything was silently dropped

## Results

> Fill in after running against the real instance. These results decide:
> whether libSQL frame replication is safe for our multi-writer flow, or
> whether the TUI needs to move to row-level sync (the same Hrana approach
> as mobile) with explicit LWW.

| Scenario | sync() result | Replica state | Remote state | Verdict |
|----------|---------------|---------------|--------------|---------|
| S1 | | | | |
| S2 | | | | |
| S3 | | | | |
| S4 | | | | |
| S5 | | | | |

## Decision tree

- **S1 both survive + S2/S3 newer-timestamp wins** → frame replication
  behaves like LWW; keep the current architecture, document it.
- **S2/S3 remote wins unconditionally** → TUI offline edits are silently
  lost. Unacceptable: move TUI to row-level sync (Hrana client with the
  same LWW merge as mobile), keep the replica for read speed only.
- **sync() errors on divergence** → same conclusion as above, plus we
  must handle the error path before shipping sync to users.
- **S4 delete resurrects** → expected (no tombstones). Confirms
  tombstone migration (`deleted_at`) is required work, not optional.
