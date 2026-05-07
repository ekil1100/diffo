# diffo

`diffo` is a terminal Git diff review tool for local review before commit or push. It opens the current working tree diff by default, including untracked files, supports `git diff`-style targets, tracks file-level reviewed state, stores inline comments, and exposes review data through script-friendly CLI commands.

The project is implemented in Zig and targets Zig `0.16.0`.

## Features

- Review unstaged, staged, and untracked working tree changes by default.
- Review explicit Git targets such as `HEAD~1..HEAD`, branches, ranges, `--cached`, and pathspecs.
- Browse diffs in a terminal UI with a right-side file list.
- Toggle inline and split diff views.
- Mark files as reviewed or unreviewed.
- Automatically invalidate reviewed state when a file patch changes.
- Add single-line or multi-line comments from the TUI or CLI.
- List comments and review state as text or JSON for scripts and agents.
- Store state outside the repository using XDG state paths.
- Use Catppuccin Mocha as the built-in default theme.
- Validate Base16/Base24 theme files.
- Apply syntax highlighting when terminal color support is available. Bundled Tree-sitter grammars are used for Zig, TypeScript/TSX, JavaScript, Rust, C, C++, and Python files, with lightweight lexical fallback for unsupported languages or unavailable source sides.

## Requirements

- Zig `0.16.0`
- Git
- A POSIX-like terminal for the interactive TUI

Install Zig with Homebrew:

```sh
brew install zig
```

If Zig is already installed through Homebrew:

```sh
brew upgrade zig
zig version
```

## Build

```sh
zig build
```

The executable is written to:

```sh
zig-out/bin/diffo
```

Run tests:

```sh
zig build test
```

## Quick Start

From inside a Git repository:

```sh
diffo
```

Or, from this project checkout:

```sh
./zig-out/bin/diffo
```

Review an explicit target:

```sh
diffo HEAD~1..HEAD
diffo main...feature
diffo --cached
diffo -- src/
```

When stdout is not a TTY, `diffo` renders a static diff screen instead of entering the interactive alternate screen.

## Interactive Keys

| Key | Action |
| --- | --- |
| `j` / `k` | Move down / up within the current file diff |
| `↑` / `↓` | Move down / up within the current file diff |
| `G` / `gg` | 跳到当前文件 diff 的底部 / 顶部 |
| `PageUp` / `PageDown` | Scroll by a larger step |
| `J` / `K` | Move to next / previous file |
| `n` / `N` | Jump to next / previous change |
| `z` | Toggle the fold at the cursor |
| `Z` | Toggle all folds in the current file |
| `v` | Toggle stacked / split diff view |
| `r` | Toggle current file reviewed state |
| `c` | Add a comment at the current diff line |
| `V` | Start a multi-line selection |
| `u` | Jump to the first unreviewed file |
| `?` | Toggle help |
| `q` | Quit |

Mouse support is enabled in the TUI:

- Scroll wheel over the diff scrolls the current file.
- Scroll wheel over the file tree moves between files.
- Clicking a diff row moves the cursor.
- Clicking a file-tree row opens that file.

When running inside tmux, mouse events must be enabled in tmux as well:

```tmux
set -g mouse on
```

## CLI Commands

Show help:

```sh
diffo --help
```

List comments:

```sh
diffo comments list
diffo comments list --file src/main.zig
diffo comments list --json
```

Get one comment:

```sh
diffo comments get cmt_0123456789abcdef
diffo comments get cmt_0123456789abcdef --json
```

清理当前 review target 中已经过期的注释（锚点状态为 `stale` 或 `missing`）：

```sh
diffo comments clean
diffo comments clean --dry-run
diffo comments clean --file src/main.zig
diffo comments clean --json
```

Add a comment without opening the TUI:

```sh
diffo comments add --file src/main.zig --line 42 --body "Check this branch."
diffo comments add --file src/main.zig --line 42 --end 45 --body "Consider extracting this block."
```

Show review status:

```sh
diffo review status
diffo review status --file src/main.zig
diffo review status --json
```

Mark a file:

```sh
diffo review mark --file src/main.zig --reviewed
diffo review mark --file src/main.zig --unreviewed
```

Theme commands:

```sh
diffo themes list
diffo themes validate path/to/theme.yaml
```

Debug Git command failures:

```sh
diffo --debug-git
diffo review status --debug-git
```

## JSON Output

Review status output includes stable top-level fields:

```json
{
  "schema_version": 1,
  "repository_id": "repo_...",
  "review_target_id": "target_...",
  "files": [
    {
      "file_path": "src/main.zig",
      "status": "unreviewed",
      "patch_fingerprint": "sha256:...",
      "comment_count": 0
    }
  ]
}
```

Comment output includes file path, line range, side, body, author, match status, and anchor data:

```json
{
  "comment_id": "cmt_...",
  "file_path": "src/main.zig",
  "start_line": 42,
  "end_line": 45,
  "side": "new",
  "body": "Consider extracting this block.",
  "author": "like",
  "match_status": "exact",
  "anchor": {
    "hunk_header": "@@ -40,3 +42,6 @@",
    "patch_fingerprint": "sha256:...",
    "stable_line_ids": ["line_..."]
  },
  "review_target_id": "target_..."
}
```

`match_status` can be:

- `exact`: the stored patch fingerprint still matches.
- `stale`: the file still exists in the current diff, but the patch changed.
- `missing`: the file no longer exists in the current diff.
- `relocated`: reserved for future relocation support.

## Storage

`diffo` does not write review data into the Git working tree. State is stored under:

```text
${XDG_STATE_HOME:-$HOME/.local/state}/diffo/repos/<repo_id>/
```

Current files:

```text
comments.json
review-states.json
```

Writes use a temporary file followed by rename, so interrupted writes should not corrupt repository contents.

## Current V1 Limits

- Tree-sitter highlighting is currently bundled for Zig files only; unsupported languages, explicit targets, large files, and unavailable source sides fall back to lightweight lexical highlighting.
- Comment relocation is minimal: comments become `stale` when the patch fingerprint changes.
- Runtime theme switching and full config loading are not implemented yet.
- Mouse support covers click selection and scroll wheel navigation; richer filtering/search can be added later.

## Development

Format sources:

```sh
zig fmt build.zig src/*.zig
```

Run the normal validation loop:

```sh
zig build test
zig build
```
