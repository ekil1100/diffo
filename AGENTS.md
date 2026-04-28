# Agent Guide

Read `README.md` first for setup, build commands, CLI usage, and TUI keys.

## Scope

- Use Zig `0.16.0`.
- Keep changes small and aligned with the existing modules.
- Prefer stdlib and local helpers; avoid new dependencies unless clearly needed.
- Do not replace native Git diff behavior with a custom diff engine.
- Do not revert user changes unless explicitly asked.

## Code Map

- `src/git.zig`: Git discovery and snapshot loading.
- `src/diff.zig`: patch parsing and diff models.
- `src/store.zig`: review state and comments.
- `src/tui.zig`: TUI loop, input, rendering.
- `src/tui_view.zig`: visual rows, folds, change navigation.
- `src/tui_text.zig`: ANSI-aware cell width and fitting.

## TUI Pitfalls

- Syntax highlighting can emit resets; row background must be reapplied after resets.
- Status/footer rendering must occupy fixed terminal rows to avoid stale frame remnants.
- Windows builds render static output; the raw interactive TUI is POSIX-oriented.

## Agent Review Data

When acting on diffo comments, use `.agents/skills/diffo/SKILL.md`.
Use CLI JSON instead of the interactive TUI for comment retrieval.
