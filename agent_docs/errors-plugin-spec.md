# Spec: cordanui "errors" plugin — an in-TUI error log viewer

> **Hand this document to an implementing agent.** It is self-contained:
> everything needed to build and test the plugin is specified here. You do
> not need access to the cordanui source tree.

---

## 1. Goal

Build a cordanui Lua plugin named **`errors-view`** that gives the user a
page inside the TUI showing everything the host has logged to its shared
error/diagnostics table — sync failures, agent crashes, plugin install
problems, etc.

The user opens it as a command (`<leader>;` → pick "errors.show") or any
way the command system allows. The page is live: it re-reads the log every
frame while open, so new entries appear without reopening.

## 2. Runtime & constraints

- **Runtime:** `runtime = "lua"` (embedded Lua 5.4). No build step, no
  binary, no subprocess protocol.
- **Data access is API-only.** The plugin has NO database access. All reads
  go through the host API `cord.errors` (specified below). Do not attempt
  file paths, `os.execute`, or SQLite — none of that reaches the host DB.
- **Never break the host.** Wrap risky calls in `pcall`. A Lua error during
  `draw` surfaces to the user as a status-line failure; avoid it by keeping
  `draw` total (no raises).
- Sandbox is NOT enforced yet (`os`/`io` exist) but using them for this
  plugin is out of spec. Pure logic + host APIs only.

## 3. Deliverable layout

```
errors-view/
├── cordanui.toml
└── main.lua
```

### `cordanui.toml` (exact)

```toml
runtime = "lua"

[plugin]
name = "errors-view"
version = "0.1.0"
description = "View the host error log in a live panel"

[capabilities]
command = true
```

Notes:
- `runtime` MUST be at the top of the file, BEFORE any `[section]` header,
  or TOML silently attaches it to `[plugin]` and the host treats the plugin
  as a missing binary.
- No `[build]` section. No `[[field]]` settings — this plugin needs none.
- Repo name should be `errors-view` so the plugin manager can detect it.

## 4. Host API reference

### 4.1 `cord.errors`

| Call | Returns | Notes |
|---|---|---|
| `cord.errors.list(limit?)` | array of entries | Newest first. `limit` optional integer, default 200. |
| `cord.errors.clear()` | `true` | Deletes ALL entries. Destructive — confirm first. |

Each entry is a table:

```lua
{
  created_at = "2026-08-26T05:48:46+00:00",  -- ISO 8601 string
  context    = "sync",                       -- subsystem tag: sync | agent | plugin | ...
  message    = "replica sync failed",        -- one-line summary
  detail     = "turso unreachable ...",      -- string OR nil
}
```

If `cord.errors` is unavailable (host too old), calls raise a Lua error.
Detect gracefully:

```lua
local ok, list = pcall(cord.errors.list, 200)
if not ok then error("this host has no cord.errors API") end
```

### 4.2 `cord.ui.show_panel` — the page itself

```lua
local sel = 1  -- plain locals persist across frames (your view model)

cord.ui.show_panel{
  title = "Errors",
  draw = function()
    -- Called EVERY frame. Must be fast and must not raise.
    return { widget, widget, ... }   -- array of widgets, or a single widget
  end,
  on_key = function(key)
    -- Return true if handled (host redraws), false/nil to pass through.
    -- Unhandled Esc closes the panel automatically.
    return false
  end,
}
```

Widget vocabulary (all the page needs):

- `{ content = "..", fg = "role"?, bold = bool? }` — one text line. `fg`
  takes a style variable name: use `"error"` for failure rows, `"primary"`
  for headers, `"outline_variant"` for metadata/timestamps.
- `{ items = {".."}, highlight = n? }` — list with 1-based highlighted row.
- `{ children = {widget, ...} }` — vertical stack (nest freely).

Key names delivered to `on_key`: characters as themselves (`"j"`, `"x"`),
named keys `"up"`, `"down"`, `"enter"`, `"esc"`, etc., chords like
`"ctrl+d"`.

Lifecycle: one panel at a time — opening another replaces yours; closing is
via `cord.ui.close_panel()` or unhandled Esc. State lives in upvalues.

### 4.3 `plugin.commands` — how the user opens it

```lua
plugin.commands = {
  ["errors.show"] = { run = M.show, desc = "Show the error log" },
}
```

- Command names are global across all plugins — keep the `errors.` prefix.
- `run` may return a string; it's shown on the host status line.
- Users run it via `<leader>;`, type to filter, Enter.

## 5. Required behaviour

1. **Command entry**: `errors.show` opens the panel. If `cord.errors.list`
   fails (old host), do not open the panel; surface a clean message instead.
2. **Empty state**: zero entries → render one line
   `"no errors logged"` (fg `outline_variant`). Still allow `x` (harmless)
   and Esc.
3. **Rows** (newest first): each error renders as a stack of two lines —
   - header: `HH:MM:SS [context] message` with the timestamp trimmed from
     the ISO string; fg `error`, bold when it's the cursor row;
   - detail (only when present): truncated to ~100 chars per line, wrapped
     onto multiple lines if longer; fg `outline_variant`.
4. **Navigation**: `j/k/up/down` move a cursor over *entries* (not lines);
   `highlight` reflects the selected row. Scrolling past the visible area
   is your responsibility — maintain a scroll offset upvalue so long logs
   work with >20 entries. `g/G` or `Home/End` jump to first/last if cheap.
5. **Refresh**: `draw` re-calls `cord.errors.list` every frame. To keep
   frames cheap, re-list only when either (a) ≥250 ms since last fetch, or
   (b) the user just pressed `r` (force refresh). Cache results between
   fetches in an upvalue.
6. **Clear**: `x` asks `cord.ui.confirm{ title="Clear errors",
   message="delete all N logged errors?" }`. Only on `true` call
   `cord.errors.clear()`. Show the resulting count via the panel itself
   (it will simply become empty) — dialogs cannot be opened from inside
   `on_key`'s caller if another dialog is open, so wrap in `pcall` and
   ignore refusal.
7. **Close**: `q` and unhandled `Esc` close the panel (`Esc` may also just
   be returned as `false` — auto-close handles it).
8. **Count badge**: the first line of the page shows
   `"N errors (last: <relative time>)"`, e.g. `"12 errors (last: 3m ago)"`.
   Relative time: <60s "just now", <60m "Xm ago", else "Xh ago".

## 6. Implementation skeleton (may be adapted, semantics may not)

```lua
-- main.lua
plugin = {}

local M = {}

function M.show()
  local ok = pcall(function() assert(cord.errors ~= nil) end)
  if not ok then return "✖ this host has no cord.errors API" end

  local cache, fetched_at = {}, 0
  local cursor, scroll = 1, 0

  local function refresh(force)
    local now = os.clock()
    if force or now - fetched_at >= 0.25 then
      local ok, rows = pcall(cord.errors.list, 200)
      if ok then cache = rows; fetched_at = now end
    end
  end

  cord.ui.show_panel{
    title = "Errors",
    draw = function()
      refresh(false)
      -- ... build widgets from `cache`, apply `cursor`/`scroll` ...
    end,
    on_key = function(key)
      -- navigation, 'r' refresh, 'x' clear-with-confirm, 'q' close
    end,
  }
  return "errors view"
end

plugin.commands = {
  ["errors.show"] = { run = M.show, desc = "Show the error log" },
}

return plugin
```

## 7. Testing checklist (all must pass before shipping)

- [ ] `cordanui.toml` parses; `runtime = "lua"` is the FIRST line; repo
      named `errors-view`
- [ ] Installs via the plugin manager (`i` + GitHub link); appears active
- [ ] `<leader>;` lists `errors.show`; running it opens the panel
- [ ] Empty log shows `"no errors logged"` and does not crash on `x`
- [ ] Seed errors (e.g. stop Turso / bad URL, press `<leader>s`) appear
      newest-first with correct context tags
- [ ] New errors raised elsewhere show up within ~a second while the panel
      is open
- [ ] `j/k` move the cursor; scrolling works with >15 entries
- [ ] Detail lines truncate/wrap; entries without detail render cleanly
- [ ] `x` prompts before clearing; cancelling keeps data; confirming empties
      the view
- [ ] `q` closes; reopening works repeatedly
- [ ] Panel behaves correctly when ANOTHER plugin opens a second panel
      (yours is replaced, host stays alive)
- [ ] Host survives `draw` with malformed data (test: nil detail, very long
      messages, unicode)

## 8. Out of scope (do not build)

- Writing to the log (the host appends through its own never-fail path;
  there is intentionally no `cord.errors.add`)
- Filtering/search by context (possible future version bump)
- Any mobile-side changes — the phone has its own native errors page
