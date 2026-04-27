---
name: diffo
description: Work with diffo review data from a local Git repository. Use this skill whenever the user asks to read, summarize, act on, export, inspect, or respond to diffo comments; asks what review comments exist; asks an agent to fix code based on local review feedback; or mentions diffo comments/review state. The primary workflow is retrieving comments with `diffo comments list --json` and turning them into actionable file/line tasks.
---

# diffo

Use this skill when the task involves `diffo`, especially when the user wants an agent to read local review comments and act on them.

The main function is to retrieve comments from `diffo` and present them in a useful form for code work.

## Requirements

- Run commands from inside the target Git repository.
- Prefer a `diffo` executable on `PATH`.
- If `diffo` is not on `PATH`, use `./zig-out/bin/diffo` when present.
- If neither exists, inspect the environment before printing an install command:
  - Run `uname -s` and `uname -m`.
  - Map OS to `linux`, `macos`, or `windows`.
  - Map architecture to `x86_64` or `aarch64`.
  - Print the matching GitHub Release install command. Do not hard-code Linux x86_64 unless the environment actually is Linux x86_64.

Install command templates:

```sh
# Linux/macOS
curl -L https://github.com/ekil1100/diffo/releases/latest/download/diffo-<os>-<arch>.tar.gz | tar -xz -C /tmp && mkdir -p "$HOME/.local/bin" && cp /tmp/diffo "$HOME/.local/bin/diffo"

# Windows PowerShell
iwr https://github.com/ekil1100/diffo/releases/latest/download/diffo-windows-<arch>.zip -OutFile diffo.zip; Expand-Archive diffo.zip -DestinationPath $env:USERPROFILE\.local\bin -Force
```

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

## Interpret Comment JSON

Each comment has the fields an agent needs for code work:

- `comment_id`: stable identifier for referencing the comment.
- `file_path`: file that the comment applies to.
- `start_line`: primary line number.
- `end_line`: optional end line for a range.
- `side`: usually `new` or `old`.
- `body`: reviewer text.
- `author`: comment author.
- `match_status`: current anchor status.
- `anchor.patch_fingerprint`: patch fingerprint from the original comment anchor.
- `anchor.hunk_header`: original hunk header.
- `review_target_id`: review target the comment belongs to.

Treat `match_status` carefully:

- `exact`: the comment still applies to the current patch.
- `stale`: the file still exists in the current diff, but the patch changed. Mention this uncertainty before editing.
- `missing`: the file is no longer in the current diff. Do not invent a location; report it.
- `relocated`: reserved for future use; verify manually before editing.

## Agent Workflow

1. Confirm the current directory is the repository under review.
2. Retrieve comments with JSON output.
3. If comments are empty, say there are no diffo comments for the current review target.
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

## Useful Related Commands

Review status:

```sh
diffo review status --json
diffo review status --file <path> --json
```

Get one comment:

```sh
diffo comments get <comment-id> --json
```

Mark a file reviewed after addressing comments:

```sh
diffo review mark --file <path> --reviewed
```

Do not mark files reviewed unless the user asked for that or the task explicitly includes completing the review workflow.

## Failure Handling

- If `diffo comments list --json` fails, rerun with `--debug-git` only when the user needs debugging details.
- If the command is unavailable, explain which executable was missing and print a GitHub Release install command for the user's OS/architecture.
- If JSON parsing fails, show the raw command output briefly and stop before editing code.
- Do not open the interactive TUI to retrieve comments; use CLI JSON for agent workflows.
