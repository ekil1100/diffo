---
name: diffo
description: Work with diffo review data from a local Git repository. Use this skill whenever the user asks to read, summarize, act on, export, inspect, or respond to diffo comments; asks what review comments exist; asks an agent to fix code based on local review feedback; or mentions diffo comments/review state.
---

# diffo

通过 JSON CLI 读取本地审阅数据，并将评论转化为有依据的代码工作。不要打开交互 TUI 获取数据。

## 准备

在被审阅的仓库中执行命令。优先使用 `PATH` 上的 `diffo`；在 diffo 源码仓库内，也可使用 `./target/debug/diffo` 或 `./target/release/diffo`。

若没有可用二进制，在 diffo 源码仓库构建：

```sh
# Requires Rust/Cargo 1.88+, Git, and a C11 compiler.
cargo build --locked
# Install into ~/.cargo/bin if needed.
cargo install --path . --locked
```

不在源码仓库时，先克隆 https://github.com/ekil1100/diffo 或询问用户二进制位置。不要在被审阅的其他项目中运行上述构建命令。

## 读取评论

```sh
git rev-parse --show-toplevel
diffo comments list --json
diffo comments list --file <path> --json
diffo comments get <comment-id> --json
```

列表 envelope 为 `{schema_version, repository_id, review_target_id, comments}`。

**范围：列表包含本仓库所有 target 的评论**，包括在 `diffo HEAD^` 等显式 target 下创建的评论。顶层 `review_target_id` 是当前 target，每条评论的 `review_target_id` 可能不同；用户限定 target 时需比对。

每条评论的关键字段：

- `comment_id`：稳定评论标识。
- `file_path`、`start_line`、`end_line`、`side`：文件、行范围与 old/new 侧。
- `body`、`author`：评论正文及作者。
- `match_status`：当前锚点状态。
- `anchor.patch_fingerprint`、`anchor.hunk_header`、`anchor.stable_line_ids`：原始 patch 与行锚点。
- `review_target_id`：创建评论时的 target。

处理状态：

- `exact`：存储的 patch 指纹仍匹配，可以核对代码后处理。
- `stale`：patch 已改变，修改前指出不确定性并验证实际位置。
- `missing`：文件不再出现在当前 diff 中，不要猜测位置。
- `relocated`：为未来功能预留；目前需人工核实。

## 工作流程

1. 确认仓库位置并读取 JSON。
2. 评论为空时说明当前仓库没有保存的 diffo 评论，不输出空评论清单。
3. 按文件分组，读取每条评论附近的实际源码。
4. 若只要求总结，报告文件、行范围、评论标识和状态。
5. 若要求修复，限定修改范围为已验证的评论要求，执行项目验证命令。
6. 报告修复结果、验证和未解决的评论；不要自动清理评论或标记已审阅。

建议摘要格式：

```text
- src/foo.rs:42 [stale]
  - id: cmt_...
  - comment: ...
  - action: ...
```

## 其他命令

审阅状态：

```sh
diffo review status --json
diffo review status --file <path> --json
```

`files[].status` 为 `reviewed` 或 `unreviewed`。未保存状态或 patch 已改变时为 `unreviewed`。

添加评论：文件和行必须存在于当前工作区 target 的 diff 中，否则返回 `invalid arguments`。

```sh
diffo comments add --file <path> --line <n> [--end <n>] --body <text>
```

清理与标记：

```sh
diffo comments clean --dry-run --json
diffo comments clean
diffo comments clean --all
diffo review mark --file <path> --reviewed
```

默认清理仅删除当前 target 的 `stale` / `missing` 评论；`--all` 删除仓库所有 target 的评论。**除非用户明确要求，或任务明确包含完成审阅流程，不要执行非 dry-run 清理，也不要标记已审阅。**

## 错误处理

- Git 失败时，仅在需要诊断细节时附加 `--debug-git` 重试。
- `comments add` 的 `invalid arguments` 可能是文件或行不在 diff 中，先核对位置。
- 找不到二进制时指出尝试的路径，并使用上面的 Cargo 构建步骤。
- JSON 损坏时展示简短错误并停止修改，不重置用户数据。
- Rust 版本沿用 Zig 版本的数据目录、schema-v1 和身份算法，不需要转换已有评论。
