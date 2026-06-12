# Agent Guide

`diffo` is a terminal Git diff review tool written in Zig (`0.16.0`): it shows the
working-tree diff (or any `git diff`-style target) in a TUI, remembers which files
you have reviewed, and stores inline comments outside the repository. `README.md`
covers setup, build commands, CLI usage, and TUI keys; this file explains how the
code fits together so you can find your way quickly.

## How a Diff Becomes a Screen

Git itself is the diff engine — diffo never computes diffs, it parses what Git
prints. The pipeline:

1. `main.zig` sets up the allocator and hands argv to `cli.zig`, which dispatches
   subcommands (`comments`, `review`, `themes`) or falls through to the review flow.
2. `git.zig` discovers the repository (`git rev-parse`), then runs
   `git diff --unified=1000000` — full context, so each hunk carries the whole
   file — and merges unstaged, staged, and untracked changes.
3. `diff.zig` parses that output into the core model: `DiffSnapshot` → `DiffFile`
   → `DiffHunk` → `DiffLine`. Each file gets a `patch_fingerprint` (SHA-256 of its
   patch) and each line a `stable_line_id`; these anchor comments and review state.
4. `tui.zig` owns the event loop and all interactive state (cursor, scroll,
   selection, fold mode). Each frame, `tui_view.zig` flattens the model into
   `VisualRow`s — stacked or split layout, folded context runs, change spans for
   `n`/`p` navigation — and `tui.zig` paints them with theme colors and syntax
   highlights.

When stdout is not a TTY (and always on Windows), the same rows are rendered once
as static output instead of entering the alternate screen.

## Code Map

Entry and plumbing:

- `src/main.zig` — entry point; allocator setup and error reporting.
- `src/cli.zig` — argument parsing and subcommand dispatch.
- `src/root.zig` — module root re-exporting the public API.
- `src/util.zig` — hashing, timestamps, env lookups, small string helpers.

Diff pipeline:

- `src/git.zig` — repository discovery, snapshot loading, file-blob fetching.
- `src/diff.zig` — patch parsing and the diff data model.
- `src/inline_diff.zig` — word-level highlights inside changed line pairs.

Persistence:

- `src/store.zig` — comments and review state, loaded from and saved to JSON.

Rendering:

- `src/tui.zig` — event loop, input decoding (keys, SGR mouse), frame painting.
- `src/tui_view.zig` — diff model → visual rows, folds, change navigation.
- `src/tui_text.zig` — ANSI-aware cell width and fitting.
- `src/theme.zig` — colors as ANSI SGR sequences; Base16/Base24 theme files.

Syntax highlighting:

- `src/syntax.zig` — highlight model; merges token spans into colored text.
- `src/syntax_cache.zig` — lazy per-file/per-side cache of parse results.
- `src/syntax_query.zig` — runs tree-sitter queries, maps captures to line spans.
- `src/syntax_grammars.zig` — registry of bundled grammars and their queries.
- `src/syntax_queries/*.scm` — the highlight queries themselves.
- `src/tree_sitter.zig` — thin FFI wrapper over the vendored tree-sitter C library.

## Persistence Model

Review data never touches the working tree. It lives under
`${XDG_STATE_HOME:-$HOME/.local/state}/diffo/repos/<repo_id>/` as `comments.json`
and `review-states.json`. Identity is content-derived: `repo_id` hashes the
repository's real path, `target_id` hashes the normalized review spec, and
comments anchor to a `patch_fingerprint` plus `stable_line_id`s. When a file's
patch changes, its reviewed state and comments invalidate automatically —
`match_status` moves from `exact` to `stale` (patch changed) or `missing` (file
left the diff). Writes go to a temp file followed by rename.

## What Is Easy to Trip Over

- Syntax highlighting emits SGR resets mid-line, so the row background must be
  reapplied after every reset; `tui_text.fitCell` keeps SGR sequences and drops
  everything else when measuring width.
- Status and footer rendering occupy fixed terminal rows; shrinking them leaves
  stale fragments from the previous frame.
- The interactive TUI is POSIX-oriented; Windows builds always render statically.
- Tests live inline in the source files next to what they test; `zig build test`
  runs both the module and executable test steps. The validation loop is
  `zig build test` then `zig build`, with `zig fmt build.zig src/*.zig` for style.
- A single allocator flows down from `main`; tests use `std.testing.allocator`,
  which fails on leaks — be deliberate with `errdefer` when ownership transfers.
- `build.zig` compiles the vendored tree-sitter runtime and each grammar's C
  sources; adding a language means a vendored parser, a `syntax_grammars.zig`
  entry, and a query file. Beyond that, the project deliberately sticks to the
  Zig standard library.

## Acting on Review Data

When working with diffo comments programmatically, follow
`.agents/skills/diffo/SKILL.md` and use the JSON CLI (`diffo comments list --json`,
`diffo review status --json`) rather than the interactive TUI.
