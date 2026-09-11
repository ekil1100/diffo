# Agent 指南

`diffo` 是 Rust 终端 Git diff 审阅工具。用户功能、按键、安装和验证命令见 `README.md`；依赖和最低 Rust 版本以 `Cargo.toml` 为准。

## 从 Git 到屏幕

1. `src/main.rs` 处理入口和退出错误，`src/cli.rs` 分派 `comments`、`review`、`themes`，其余参数交给 Git diff。
2. `src/git.rs` 发现仓库，执行 `git diff --unified=1000000`，按未暂存、已暂存、未跟踪的顺序合并。Git 是唯一 diff 引擎，不在应用内重新计算文件 diff。
3. `src/diff.rs` 将原始 patch 字节解析为 `DiffSnapshot → DiffFile → DiffHunk → DiffLine`；生成 patch 指纹和稳定行标识。
4. `src/tui_view.rs` 按 stacked/split、折叠状态生成借用 diff 行的 `VisualRow` 和改动区间。
5. `src/tui.rs` 管理交互状态、滚动和绘制；`src/tui_editor.rs` 负责浮动评论编辑器。非 TTY 或 Windows 只渲染一次静态屏幕。

## 模块边界

- `src/store.rs`：评论和审阅状态的 schema-v1 JSON 读写、过滤与失效判断。
- `src/tui_input.rs`：Unix stdin 的唯一消费者，增量解析 UTF-8、CSI/SS3、SGR mouse 和括号粘贴；终端模式和事件类型使用 crossterm。
- `src/tui_text.rs`：ANSI 净化、终端单元宽度、定宽文本。仅保留合法 SGR，过滤其他终端控制序列。
- `src/inline_diff.rs`：有界 token LCS，计算替换行内部的字节高亮区间。
- `src/theme.rs`：Catppuccin 色彩 token、ANSI 输出和 Base16/Base24 校验。
- `src/syntax_cache.rs`：按文件/侧缓存源文本高亮，包括不可用和超大文件的负缓存。
- `src/syntax_query.rs`：官方 Tree-sitter Rust bindings、query 执行、行级 span。
- `src/syntax_grammars.rs`、`src/syntax_queries/`：grammar 注册表及查询。
- `build.rs`：通过 `cc` 编译 `vendor/` 内的 grammar C 源；runtime 来自 `tree-sitter` crate，不另编译 vendored runtime。
- `src/util.rs`：SHA-256、时间、参数规范化、作者名和原始字节到显示文本的转换。

## 数据身份与安全

评论从不写入工作区，而位于 `${XDG_STATE_HOME:-$HOME/.local/state}/diffo/repos/<repo_id>/`。Rust 实现沿用 Zig 版本的目录、schema 和身份算法，已有数据不做迁移或重置。

- `repo_id` 来自仓库 realpath；`target_id` 来自 target kind 与规范化参数。
- `patch_fingerprint` 必须 hash 原始 patch 字节；不要先转为 lossy UTF-8、改换行或重新序列化。
- 稳定行标识含路径、行类型、两侧行号（缺失值写 `null`）和原始行文本。
- 合并同路径 patch 时用单 LF 拼接，保留输入顺序和首个文件元数据。
- `comments list` 遍历仓库所有 target；match refresh 和默认清理只针对当前 target。
- 每个状态文件分别同目录临时写、sync、原子替换；落盘成功后才更新内存。损坏数据应报错，不能覆盖成空集合。
- 子进程任一输出流超限后，先终止整个进程组/Windows Job，再回收管道和进程，防止 textconv 后代持有管道造成阻塞。

## 易错点与验证

- 语法高亮会在行内插入 SGR reset，必须重施行背景和 inline 背景；状态栏/footer 每帧完整重绘。
- 行号是逻辑行，滚动和鼠标使用换行后的视觉高度；预测与绘制应共享几何逻辑。
- 选择范围在布局或折叠结构变化后清除，避免评论锚点错位。
- 终端 guard 必须覆盖部分初始化、正常退出、I/O 错误和 panic；SIGINT/SIGTERM 只设原子标志，由事件循环清理。
- 添加语言时同时更新 vendored parser、`build.rs`、grammar 注册和 query，并用真实 parser/query 测试。
- 单元测试紧邻源码；`tests/cli.rs`、`tests/git_pipeline.rs` 和 `tests/migration.rs` 验证跨模块行为。所有持久化测试必须用临时目录，CLI 子进程隔离 `HOME`、`XDG_STATE_HOME` 和 Git 配置。
- `tests/terminal.py` 验证真实 PTY 输入和恢复；需要该类验证时用 `just test-terminal`。测试桩拦截剪贴板命令，不修改用户剪贴板。
- 修改后按风险执行 `cargo fmt --all -- --check`、`cargo clippy --locked --all-targets -- -D warnings`、`cargo test --locked` 和 `cargo build --locked`。

`.agents/` 中早期设计和审计文档是 Zig 阶段的历史记录，不能作为当前实现或构建说明；当前代码地图以本文为准。

## 操作审阅数据

用户要求读取、总结或处理 diffo 评论/审阅状态时，先读 `.agents/skills/diffo/SKILL.md`，使用 `diffo comments list --json`、`diffo review status --json`，不要用交互 TUI 获取数据。
