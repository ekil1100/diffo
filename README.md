<p align="center">
  <img src="docs/assets/diffo.png" alt="diffo wordmark" width="300">
</p>

# diffo

`diffo` 是用 Rust 实现的终端 Git diff 审阅工具，适合在提交、推送或创建 PR 前进行本地审阅。它默认合并展示未暂存、已暂存和未跟踪文件的改动，记录文件审阅状态与行内评论，并提供面向脚本和 agent 的 JSON CLI。

审阅数据保存在仓库之外，不污染工作区。文件 patch 变化后，原审阅状态自动失效。

## 功能

- 支持工作区、分支、提交、两点/三点范围、`--cached` 和 pathspec。
- 默认全宽代码区，可收起的左侧文件导航，stacked / split 双布局。
- 光标优先导航、改动跳转、上下文折叠、鼠标导航与拖选、复制选中 diff。
- 单行/多行评论、已审阅标记、过期评论清理。
- 默认 Catppuccin Mocha 主题，Base16/Base24 主题文件校验。
- 内置 Tree-sitter 高亮：Zig、TypeScript/TSX、JavaScript、Rust、C、C++、Python、GN。

## 安装与构建

需要：

- Rust / Cargo **1.88 或更新版本**，可通过 [rustup](https://rustup.rs/) 安装。
- Git。
- C11 编译器，用于编译内置 Tree-sitter grammar；**不再需要 Zig**。
- 交互模式需要 POSIX 终端；Windows 使用静态输出。

C 编译器可使用 macOS 的 Xcode Command Line Tools、Linux 的 GCC/Clang，或 Windows 的 Visual Studio Build Tools（安装 C++ 工具与 Windows SDK）。

```sh
cargo build --locked
cargo run --locked -- --help
```

开发二进制位于 `target/debug/diffo`，Windows 下为 `diffo.exe`。

安装优化版本到 `~/.cargo/bin`：

```sh
cargo install --path . --locked
```

或通过 `just` 安装到 `~/.local/bin`：

```sh
just install
```

手动安装：

```sh
cargo build --release --locked
mkdir -p ~/.local/bin
cp target/release/diffo ~/.local/bin/diffo
```

确保相应安装目录已加入 `PATH`。首次构建需要下载 Cargo 依赖；`Cargo.lock` 固定依赖版本。

## 快速开始

在任意 Git 仓库中运行：

```sh
diffo
diffo HEAD~1..HEAD
diffo main...feature
diffo --cached
diffo -- src/
```

标准输入或标准输出不是 TTY 时，以及 Windows 上，程序只输出一次静态审阅屏幕，不进入交互模式。

## 交互按键

采用 **cursor-first（光标优先）** 交互：`>` 标记当前焦点中的光标，`|` 标记选择范围。底栏用 `CODE` / `SELECT`、`FILES`、`COMMENTS`、`COMMENT` 区分代码、文件导航、评论阅读和评论编辑。导航先移动光标，视口仅在需要时跟随。

默认代码占满宽度。`Tab` 打开文件导航，`j/k` 仅选择文件，`Enter` 确认打开；112 列以上显示可保留的左栏，更窄时使用全宽选择器，确认后收起。当前打开的文件用 `*` 标记。切回文件会恢复本次会话中的光标、滚动、布局和折叠状态，不写入审阅状态文件。

| 按键 | 操作 |
| --- | --- |
| `Tab` | 打开文件导航；在文件导航中再次按下可关闭 |
| `Enter` | 在代码区打开当前行评论；在文件导航中确认打开；在评论阅读区返回代码 |
| `j` / `k`、`↓` / `↑` | 移动代码光标 / 选择文件 / 滚动评论，取决于当前焦点 |
| `G` / `gg`、`End` / `Home` | 光标跳至末尾 / 开头 |
| `PageUp` / `PageDown` | 代码按可用高度翻页，保留一行重叠并计入换行；辅助区按各自高度翻页 |
| `J` / `K` | 下一个 / 上一个文件 |
| `n` / `p` | 下一个 / 上一个改动 |
| `C` | 切换展开 / 折叠模式 |
| `z` | 在折叠标记或其已展开的上下文中切换折叠，光标保留在该折叠处 |
| `Z` | 在折叠模式切换当前文件全部折叠 |
| `v` | 切换 stacked / split |
| `r` | 切换当前文件审阅状态 |
| `c` | 为光标所在代码行或选择范围添加评论；不自动改用附近代码行 |
| `V` | 开始 / 取消多行选择，导航延伸选择范围 |
| `y` | 复制所选 diff 行或当前行 |
| `Esc` | 清除选择，或关闭当前辅助区并返回代码；代码区无选择时也可收起左栏 |
| `u` | 跳至第一个未审阅文件 |
| `?` | 打开 / 关闭帮助面板，`j/k` 或翻页键滚动 |
| `q` / `Ctrl+C` | 退出 |

切换布局或折叠结构会清除选择，并按代码身份重新定位光标；被隐藏的代码定位到对应折叠标记。split 的一行是左右配对行，复制包含两侧，评论优先锚定新侧（只有旧侧时使用旧侧）。

评论不会插入代码流，也不会随光标经过而自动撑开。`Enter` 查看评论，`c` 在底部固定高度的区域编辑；标题显示文件、旧/新侧及行范围，编辑期间锁定锚点。保存或取消后关闭编辑区，回到代码。打开评论需要至少 20 列代码宽度、7 行终端高度。

评论编辑器支持 UTF-8、方向键、Backspace/Delete 和括号粘贴：`Enter` 保存，`Shift+Enter` 换行，`Esc` 取消，`Ctrl+C` 退出程序。换行需要终端发送可区分的 Shift+Enter 编码；单次括号粘贴上限为 1 MiB。

鼠标操作：

- 在 diff 区滚轮每次移动光标 3 个屏幕行，视口按需跟随。
- 文件导航内的滚轮只改变候选文件；单击文件立即打开。
- 单击 diff 行返回代码焦点并移动光标，拖动选择多行。
- 评论区单击或滚轮进入评论阅读焦点，滚动评论不会移动代码。

tmux 需要启用鼠标：

```tmux
set -g mouse on
```

复制优先使用系统剪贴板（`pbcopy`、`wl-copy`、`xclip` 或 `xsel`）；可用性还取决于终端的 OSC 52 支持与 tmux 配置。

## CLI

```sh
diffo --help

# List comments from every review target in this repository.
diffo comments list
diffo comments list --file src/main.rs
diffo comments list --json

# Read one comment.
diffo comments get cmt_0123456789abcdef
diffo comments get cmt_0123456789abcdef --json

# Add a comment anchored to the current working-tree diff.
diffo comments add --file src/main.rs --line 42 --body "Check this branch."
diffo comments add --file src/main.rs --line 42 --end 45 --body "Consider extracting this block."

# Inspect or change file review state.
diffo review status
diffo review status --file src/main.rs --json
diffo review mark --file src/main.rs --reviewed
diffo review mark --file src/main.rs --unreviewed

# Preview expired-anchor cleanup, or delete comments.
diffo comments clean --dry-run --json
diffo comments clean
diffo comments clean --file src/main.rs
diffo comments clean --all

# Theme tools and Git diagnostics.
diffo themes list
diffo themes validate path/to/theme.yaml
diffo --debug-git
diffo review status --debug-git
```

`comments list` 包含本仓库所有 target 的评论。`comments clean` 默认只清理当前工作区 target 中 `stale` / `missing` 的评论；`--all` 删除本仓库所有 target 的评论，仍可用 `--file` 过滤。删除前建议先加 `--dry-run`。

## JSON 输出

审阅状态：

```json
{
  "schema_version": 1,
  "repository_id": "repo_...",
  "review_target_id": "target_...",
  "files": [
    {
      "file_path": "src/main.rs",
      "status": "unreviewed",
      "patch_fingerprint": "sha256:...",
      "comment_count": 0
    }
  ]
}
```

评论列表的顶层字段为 `{schema_version, repository_id, review_target_id, comments}`。其中每条评论包含：

```json
{
  "comment_id": "cmt_...",
  "file_path": "src/main.rs",
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

列表顶层 `review_target_id` 表示当前 target，每条评论自身的 target 可能不同。

| `match_status` | 含义 |
| --- | --- |
| `exact` | 文件 patch 指纹仍匹配 |
| `stale` | 文件仍在 diff 中，但 patch 已变化 |
| `missing` | 文件已不在当前 diff 中 |
| `relocated` | 为未来的评论重定位预留 |

## 存储与 Zig 版本迁移

存储目录和 JSON schema 保持不变，**已有 Zig 版本的数据无需转换**：

```text
${XDG_STATE_HOME:-$HOME/.local/state}/diffo/repos/<repo_id>/
  comments.json
  review-states.json
```

仓库 realpath、target、patch 和行标识继续使用相同的 SHA-256 规则。迁移实现语言本身不会令已有评论/审阅失效；实际 patch 或仓库位置变化仍按原规则处理。

每个 JSON 文件先写入同目录临时文件，同步后原子替换；写入失败不会提交内存中的变更。损坏或不支持的 schema 不会被自动重置。两个文件不是一个跨文件事务，也不支持多进程同时写入合并。

## 当前限制

- 大文件、未支持的语言和无法取得源文本的侧不高亮；高亮源上限 512 KiB。
- Git stdout 上限 200 MiB，stderr 上限 20 MiB；超限会终止 Git 及其辅助进程并报错。
- 保留原 Git C-quoted 路径处理方式；CLI 参数必须是有效 UTF-8。
- 混合 staged/unstaged 文件、某些额外 Git 选项的源侧推断仍有限制。
- 评论重定位、运行时主题切换和完整配置加载尚未实现。
- Base16/Base24 校验检查颜色槽，并非通用 YAML 解析器。

## 开发验证

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked
cargo build --locked
```

POSIX 上的真实 PTY 回归测试需要 Python 3：

```sh
python3 tests/terminal.py
```

它在临时 Git 仓库与独立 `HOME` / `XDG_STATE_HOME` 中验证键盘、鼠标、评论、resize 和终端恢复，不访问真实评论或系统剪贴板。`just test-terminal` 还会执行需 PTY 的 panic 与输入错误探针。

发布构建：

```sh
cargo build --release --locked
just release
```

跨平台构建需安装对应 Rust target、C 编译器与链接器；发布 CI 使用原生 runner 或 Windows MSVC 交叉工具链，不再依赖 Zig 的交叉编译器。
