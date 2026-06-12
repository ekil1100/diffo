---
name: diffo
description: Work with diffo review data from a local Git repository. Use this skill whenever the user asks to read, summarize, act on, export, inspect, or respond to diffo comments; asks what review comments exist; asks an agent to fix code based on local review feedback; or mentions diffo comments/review state.
---

# diffo

Use this skill when the task involves `diffo`, especially when the user wants an agent to read local review comments and act on them.

The main function is to retrieve comments from `diffo` and present them in a useful form for code work.

## Requirements

- Run commands from inside the target Git repository.
- Prefer a `diffo` executable on `PATH`.
- If `diffo` is not on `PATH`, use `./zig-out/bin/diffo` when present.
- If neither exists, diffo is built from source (there are no binary releases):

```sh
# Requires Zig 0.16.0 and Git
zig build
# binary at zig-out/bin/diffo; optionally install:
mkdir -p "$HOME/.local/bin" && cp zig-out/bin/diffo "$HOME/.local/bin/diffo"
```

Outside the diffo repository, clone https://github.com/ekil1100/diffo first or ask the user where the binary lives.

## Get Comments

First get comments as JSON:

```sh
diffo comments list --json
```

If `diffo` is not on `PATH`, use:

```sh
./zig-out/bin/diffo comments list --json
```

For one file:

```sh
diffo comments list --file <path> --json
```

The response envelope is `{schema_version, repository_id, review_target_id, comments}`.

Scope: the list covers **all comments saved for the repository**, including ones created under explicit review targets such as `diffo HEAD^`. The top-level `review_target_id` is the current review target; each comment carries its own `review_target_id`, which may differ. Compare them when the task is scoped to the current target.

## Interpret Comment JSON

Each comment has the fields an agent needs for code work:

- `comment_id`: stable identifier for referencing the comment.
- `file_path`: file that the comment applies to.
- `start_line`: primary line number.
- `end_line`: end of the range; equals `start_line` for single-line comments.
- `side`: usually `new` or `old`.
- `body`: reviewer text.
- `author`: comment author.
- `match_status`: current anchor status.
- `anchor.patch_fingerprint`: patch fingerprint from the original comment anchor.
- `anchor.hunk_header`: original hunk header.
- `anchor.stable_line_ids`: content-derived line identifiers.
- `review_target_id`: review target the comment belongs to.

Treat `match_status` carefully:

- `exact`: the comment still applies to the current patch.
- `stale`: the file still exists in the current diff, but the patch changed. Mention this uncertainty before editing.
- `missing`: the file is no longer in the current diff. Do not invent a location; report it.
- `relocated`: reserved for future use; verify manually before editing.

## Agent Workflow

1. Confirm the current directory is the repository under review (`git rev-parse --show-toplevel`).
2. Retrieve comments with JSON output.
3. If comments are empty, say there are no diffo comments saved for this repository.
4. Group comments by `file_path`.
5. For each comment, read the relevant file and inspect the nearby lines.
6. If the user asked for a summary, report comments grouped by file with line ranges and status.
7. If the user asked to fix issues, make scoped edits that address the comments, then run the project validation command when available.

## Output Format

When summarizing comments, use this format:

```text
Diffo comments:

- path/to/file.ext:12
  - id: cmt_...
  - status: exact
  - comment: ...
  - action: ...
```

For stale or missing comments, include the status inline:

```text
- src/foo.zig:42 [stale]
  - comment: ...
  - note: Patch changed since this comment was created; verify before editing.
```

When there are no comments, state exactly: "There are no diffo comments saved for this repository." — skip the block format.

## Useful Related Commands

Review status:

```sh
diffo review status --json
diffo review status --file <path> --json
```

`files[].status` is `reviewed` or `unreviewed`; files default to `unreviewed` when no state has been saved yet.

Get one comment:

```sh
diffo comments get <comment-id> --json
```

Add a comment without the TUI (the file **and** line must be part of the current review target's diff, or the command fails with `invalid arguments`):

```sh
diffo comments add --file <path> --line <n> [--end <n>] --body <text>
```

Clean up comments whose anchors expired (`stale` or `missing`), or wipe all saved comments with `--all`:

```sh
diffo comments clean --dry-run --json   # preview removals
diffo comments clean                    # remove stale/missing in current target
diffo comments clean --all              # remove every saved comment
```

Mark a file reviewed after addressing comments:

```sh
diffo review mark --file <path> --reviewed
```

Do not mark files reviewed or run `comments clean` without `--dry-run` unless the user asked for that or the task explicitly includes completing the review workflow.

## Failure Handling

- If `diffo comments list --json` fails, rerun with `--debug-git` only when the user needs debugging details.
- `invalid arguments` from `comments add` usually means the file or line is not in the current diff, not a syntax error in your flags.
- If the command is unavailable, explain which executable was missing and point to the build-from-source steps above.
- If JSON parsing fails, show the raw command output briefly and stop before editing code.
- Do not open the interactive TUI to retrieve comments; use CLI JSON for agent workflows.
