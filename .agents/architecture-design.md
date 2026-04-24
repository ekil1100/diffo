# diffo 架构设计文档

## 1. 文档信息

- 文档类型：架构设计文档
- 对应产品：diffo
- 实现语言：Zig
- 默认主题：Catppuccin
- 主题系统兼容：Base16/Base24

## 2. 架构目标

本架构设计的目标是：

1. 以 Zig 构建一个高性能、可移植、可维护的终端 Review CLI/TUI。
2. 复用 Git 原生能力，避免重复实现复杂 diff 语义。
3. 使 TUI、Git 解析、状态存储、评论系统、CLI 查询接口相互解耦。
4. 为未来扩展至更多 review target、评论状态机、远端平台集成与 Agent 工作流保留稳定边界。

## 3. 总体架构

系统采用分层 + 模块化架构，共分为六层：

1. `CLI Layer`
2. `Application Layer`
3. `Domain Layer`
4. `Infrastructure Layer`
5. `Persistence Layer`
6. `Presentation Layer`

其中：

- `CLI Layer` 负责命令行参数解析与子命令路由
- `Application Layer` 负责用例编排
- `Domain Layer` 负责 review target、diff、comment、review state 等核心模型与规则
- `Infrastructure Layer` 负责 Git、终端、文件系统、时间、序列化等基础能力
- `Persistence Layer` 负责本地数据存储
- `Presentation Layer` 负责 TUI 布局、事件循环、diff 渲染、代码高亮与主题系统

## 4. 关键设计决策

## 4.1 Git 集成策略

### 决策

优先通过调用系统 `git` 命令获取 diff、文件列表、对象信息，而不是在 V1 中自行实现完整 Git diff 引擎。

### 理由

- `git diff` 语法复杂，原生命令兼容性最好
- 可直接支持 commit、range、branch、pathspec 等语义
- 与用户预期一致，减少行为偏差
- V1 聚焦 review 体验，而非重新造 Git 内核

### 影响

- 需要构建稳定的 Git 命令适配层
- 需要处理命令错误、编码与性能问题
- 未来若需要完全脱离外部 Git，可替换为 libgit2 或原生实现，但不影响上层

## 4.2 渲染策略

### 决策

采用“结构化 diff AST + TUI 渲染器”模型，而不是直接打印原始 diff 文本。

### 理由

- 支持 inline / split 两种视图
- 支持行级光标、行选择、评论锚点与高亮
- 支持评论气泡、review 状态标记、折叠与跳转

## 4.3 状态持久化策略

### 决策

使用本地结构化存储，推荐 JSON Lines / JSON 文件 + 原子写入；如后续复杂度增加可升级为 SQLite，但 V1 以文件型存储优先。

### 理由

- 易于调试
- 便于 CLI 与 Agent 直接读取
- Zig 实现成本低
- 数据规模在 V1 阶段可控

### 推荐目录

优先遵循 XDG Base Directory Specification，并按用途拆分配置、状态、数据与缓存：

- 配置目录：`${XDG_CONFIG_HOME:-$HOME/.config}/diffo/`
- 状态目录：`${XDG_STATE_HOME:-$HOME/.local/state}/diffo/`
- 数据目录：`${XDG_DATA_HOME:-$HOME/.local/share}/diffo/`
- 缓存目录：`${XDG_CACHE_HOME:-$HOME/.cache}/diffo/`

推荐用途：

- `config.toml` 放在配置目录，用于默认主题、快捷键、diff 模式等用户偏好。
- review state、评论索引、repo 映射等可恢复但有长期价值的数据放在状态目录。
- 内置主题、外部主题副本、schema 迁移元数据等放在数据目录。
- Git diff snapshot 缓存、渲染缓存、语法高亮缓存、日志文件放在缓存目录。

macOS 与 Windows 可通过平台适配层映射到各自标准用户目录，但内部路径解析仍使用“config/state/data/cache”四类语义。仓库级索引按仓库哈希分目录存放在状态目录，不直接写入仓库工作树。

## 5. 模块划分

建议代码目录结构如下：

```text
src/
  main.zig
  cli/
    parser.zig
    commands.zig
  app/
    review_session_service.zig
    comment_service.zig
    review_state_service.zig
    theme_service.zig
    highlight_service.zig
  domain/
    review_target.zig
    diff_model.zig
    comment.zig
    review_state.zig
    repository.zig
    theme.zig
    syntax.zig
  infra/
    git/
      git_runner.zig
      git_parser.zig
      diff_loader.zig
    fs/
      paths.zig
      atomic_write.zig
    term/
      terminal.zig
      input.zig
      output.zig
    serde/
      json.zig
    syntax/
      language_detector.zig
      highlighter.zig
      grammars.zig
      tree_sitter.zig
      query_loader.zig
      ast_index.zig
    time/
      clock.zig
  store/
    repo_store.zig
    comment_store.zig
    review_state_store.zig
    config_store.zig
  tui/
    app.zig
    layout.zig
    event_loop.zig
    widgets/
      diff_view.zig
      file_tree.zig
      status_bar.zig
      comment_dialog.zig
      help_dialog.zig
    render/
      diff_renderer.zig
      split_renderer.zig
      inline_renderer.zig
      theme_mapper.zig
      syntax_renderer.zig
```

## 6. 运行模式

系统支持两类运行模式：

### 6.1 交互式模式

命令示例：

```bash
diffo
diffo --cached
diffo main...feature
```

流程：

1. 解析参数
2. 构造 `ReviewTarget`
3. 调用 Git 适配层加载 diff snapshot
4. 加载 review state 与 comments
5. 启动 TUI 事件循环
6. 响应输入并进行渲染与持久化

### 6.2 非交互查询模式

命令示例：

```bash
diffo comments list --json
diffo review status --file src/main.zig
```

流程：

1. 解析子命令
2. 加载本地存储
3. 输出文本或 JSON
4. 以稳定退出码结束

## 7. 核心领域模型

## 7.1 Repository

```text
Repository
- root_path
- repo_id
- current_branch
```

`repo_id` 建议由仓库根路径规范化后哈希生成，也可结合 Git 顶层目录真实路径。

## 7.2 ReviewTarget

```text
ReviewTarget
- kind
- raw_args
- normalized_spec
- pathspecs
- target_id
```

### `kind` 示例

- `working_tree`
- `cached`
- `commit`
- `range`
- `symmetric_range`

### 设计原则

- 保留用户原始输入
- 生成规范化表达，作为缓存与持久化键的一部分

## 7.3 DiffSnapshot

```text
DiffSnapshot
- snapshot_id
- repository_id
- review_target_id
- created_at
- files[]
```

### 文件级模型

```text
DiffFile
- path
- old_path
- status
- language
- is_binary
- hunks[]
- patch_fingerprint
- old_blob_oid
- new_blob_oid
```

### Hunk 模型

```text
DiffHunk
- header
- old_start
- old_count
- new_start
- new_count
- lines[]
```

### Line 模型

```text
DiffLine
- kind            // context/add/delete/meta
- old_lineno
- new_lineno
- text
- display_lineno
- stable_line_id
```

`stable_line_id` 用于评论锚点、光标恢复与渲染索引。

## 7.4 ReviewState

```text
ReviewState
- repository_id
- review_target_id
- file_path
- status           // reviewed/unreviewed
- patch_fingerprint
- updated_at
```

### 自动失效规则

当当前 `patch_fingerprint` 与持久化记录不一致时，状态自动视为 `unreviewed`。

推荐 `patch_fingerprint` 来源：

- 优先文件级 patch 内容哈希
- 若无法稳定获取，则使用 `old_blob_oid + new_blob_oid + hunk headers` 组合哈希

## 7.5 Comment

```text
Comment
- comment_id
- repository_id
- review_target_id
- file_path
- anchor
- body
- author
- created_at
- updated_at
- tags[]
```

### Anchor 模型

```text
CommentAnchor
- side                // old/new/both
- start_line
- end_line
- stable_line_ids[]
- hunk_header
- context_before[]
- context_after[]
- patch_fingerprint
- match_status        // exact/relocated/stale/missing
```

### 设计说明

- 单行评论：`start_line == end_line`
- 多行评论：`start_line < end_line`
- `stable_line_ids` 用于同一 snapshot 内快速定位
- `context_before/after` 用于文件变化后的重定位

## 8. Git 基础设施设计

## 8.1 GitRunner

职责：

- 统一执行 `git` 命令
- 注入 `-C <repo>` 参数
- 捕获 stdout/stderr/exit code
- 管理超时与错误包装

推荐提供接口：

```text
run(args: []const []const u8) -> GitCommandResult
```

## 8.2 DiffLoader

职责：

- 根据 `ReviewTarget` 生成 Git 命令
- 获取文件列表、patch 内容、对象元信息
- 组装为 `DiffSnapshot`

建议命令组合：

- 文件列表：`git diff --name-status ...`
- patch 内容：`git diff --patch --find-renames ...`
- staged/unstaged 默认场景需分别读取后合并视图

## 8.3 GitParser

职责：

- 解析 `git diff --patch` 输出
- 构建 `DiffFile / DiffHunk / DiffLine`
- 识别 rename、binary、mode change 等元信息

### 解析策略

- 采用 streaming parser，避免一次性过度复制大文本
- 先按文件切分，再按 hunk 切分，最后解析行
- 元信息行如 `diff --git`、`index`、`rename from`、`rename to` 单独建模

## 9. TUI 架构设计

## 9.1 视图模型

TUI 不直接依赖底层 Git 原始输出，而依赖 `ViewModel`：

```text
ReviewScreenViewModel
- repository
- review_target
- file_items[]
- active_file_index
- diff_mode
- cursor
- selection
- visible_comments
- theme
```

## 9.2 布局

推荐默认布局：

```text
+--------------------------------------------------------------+
| Top Status Bar                                               |
+------------------------------------------+-------------------+
| Diff View                                 | File Tree         |
|                                           |                   |
|                                           |                   |
+------------------------------------------+-------------------+
| Bottom Status / Key Hints                                     |
+--------------------------------------------------------------+
```

### 布局原则

- 宽屏时默认右侧边栏
- 窄终端下自动压缩边栏宽度
- 极窄终端下允许切换“单栏模式”

## 9.3 事件循环

事件来源：

- 键盘输入
- 窗口 resize
- 定时刷新
- 后台数据变更检测

主循环：

1. 拉取事件
2. 更新状态机
3. 触发必要的数据重载
4. 最小化重绘

## 9.4 模式状态机

建议定义以下模式：

- `normal`
- `file_tree_focus`
- `comment_input`
- `multi_line_select`
- `help_overlay`

### 状态切换示意

```text
normal -> comment_input
normal -> multi_line_select
normal -> file_tree_focus
any    -> help_overlay
overlay/input -> normal
```

## 10. Diff 渲染设计

## 10.1 Inline Renderer

特点：

- 以单列形式按行渲染
- 删除、增加、上下文行使用不同底色/前景色
- 评论图标与行号可直接叠加

## 10.2 Split Renderer

特点：

- 左列显示 old side
- 右列显示 new side
- 上下文行对齐展示
- 删除/新增区域允许空位占位

### 关键点

- 需要中间 gutter 以承载光标、评论标识、选区提示
- 行映射不能仅依赖物理行号，应依赖逻辑对齐行

## 10.3 评论渲染

评论展示分两层：

1. 行侧边图标或标记
2. 弹出式评论面板/底部评论列表

V1 建议不在主 diff 内直接展开长评论正文，以免破坏浏览密度；优先采用侧边标记 + 详情弹层。

## 10.4 代码高亮渲染

### 10.4.1 高亮分层

渲染管线应将 diff 语义高亮与代码语法高亮分层处理：

1. Diff parser 先产出 `DiffLine.kind`
2. Tree-sitter parser 解析 old/new 文件内容并通过 highlight query 产出 token spans
3. Renderer 将 token 色彩与 diff 行背景组合为最终终端样式

这种方式可以确保新增/删除行的背景不会被语法 token 覆盖，也能让主题系统统一管理色彩。

### 10.4.2 SyntaxToken 模型

```text
SyntaxToken
- kind          // keyword/string/comment/type/function/number/operator/plain
- start_col
- end_col
- text
```

### 10.4.3 HighlightedLine 模型

```text
HighlightedLine
- line_id
- language
- tokens[]
- source_hash
```

`source_hash` 用于避免重复高亮相同内容。

### 10.4.4 SyntaxTree 模型

```text
SyntaxTree
- file_path
- language
- side          // old/new/worktree
- source_hash
- root_node
- parse_status // parsed/partial/failed/skipped
```

AST 默认不直接暴露给渲染层，渲染层只消费 `HighlightedLine`。后续符号导航、函数级折叠与结构化评论锚点重定位可以通过 `ast_index.zig` 读取受控 AST 索引。

### 10.4.5 语言识别

`LanguageDetector` 负责根据文件路径、文件名、shebang 与配置判断语言：

```text
detect(file_path, first_line, config) -> ?LanguageId
```

识别失败时返回 null，渲染器退回纯 diff 高亮。

### 10.4.6 高亮引擎选型

V1 采用 Tree-sitter 作为语法高亮与 AST 引擎：

- `tree_sitter.zig` 封装 C ABI、parser 生命周期、解析超时与错误处理
- `grammars.zig` 管理内置或可选 grammar 的注册表
- `query_loader.zig` 加载每种语言的 highlight query
- `highlighter.zig` 将 Tree-sitter capture 映射为 `SyntaxToken`

当 grammar、query、文件内容或解析结果不可用时，`Highlighter` 必须降级为纯 diff 语义高亮。若未来需要快速 fallback，可在同一接口下增加轻量 lexer，但不是 V1 主路径。

### 10.4.7 Diff 行映射

Tree-sitter 更适合解析完整文件而不是孤立 diff 片段。渲染时应优先读取 old/new 两侧完整文件内容，分别解析后将 token spans 映射回 diff 行：

- working tree diff：new side 读取工作区文件，old side 读取 Git 对象
- staged diff：new side 读取 index 内容，old side 读取 HEAD 或比较基准
- commit/range diff：old/new side 分别读取对应 Git 对象中的文件内容
- 当无法读取完整内容时，可解析 hunk 片段并标记为 `partial`

## 11. 文件树设计

文件树节点结构：

```text
FileTreeItem
- file_path
- status
- reviewed
- comment_count
- patch_fingerprint
- is_active
```

支持的排序策略：

- 默认按 Git 输出顺序
- 后续可扩展：未审阅优先、评论优先、路径排序

## 12. 评论与审阅状态存储设计

## 12.1 存储目录

建议目录：

```text
<data_root>/
  version.json
  repos/
    <repo_id>/
      config.json
      comments.json
      review-states.json
```

## 12.2 `comments.json`

建议结构：

```json
{
  "schema_version": 1,
  "comments": [
    {
      "comment_id": "cmt_01",
      "repository_id": "repo_xxx",
      "review_target_id": "target_xxx",
      "file_path": "src/main.zig",
      "anchor": {
        "side": "new",
        "start_line": 42,
        "end_line": 45,
        "stable_line_ids": ["l1", "l2", "l3", "l4"],
        "hunk_header": "@@ -40,3 +42,6 @@",
        "context_before": ["foo()", "bar()"],
        "context_after": ["baz()"],
        "patch_fingerprint": "sha256:...",
        "match_status": "exact"
      },
      "body": "这里建议拆分函数。",
      "author": "like",
      "created_at": "2026-04-23T10:00:00Z",
      "updated_at": "2026-04-23T10:00:00Z"
    }
  ]
}
```

## 12.3 `review-states.json`

建议结构：

```json
{
  "schema_version": 1,
  "states": [
    {
      "repository_id": "repo_xxx",
      "review_target_id": "target_xxx",
      "file_path": "src/main.zig",
      "status": "reviewed",
      "patch_fingerprint": "sha256:...",
      "updated_at": "2026-04-23T10:10:00Z"
    }
  ]
}
```

## 12.4 写入策略

- 内存中修改后，批量写回对应 JSON 文件
- 写入采用临时文件 + rename 原子替换
- 启动时检测 schema 版本
- 文件损坏时保留备份并给出恢复提示

## 13. 评论重定位策略

## 13.1 目标

在文件发生轻微变更后，尽量保留评论的可追踪性。

## 13.2 重定位流程

1. 优先按 `stable_line_ids` 在当前 snapshot 匹配
2. 若失败，按 `hunk_header + context_before/after` 搜索邻近区域
3. 若仍失败，按 `start_line/end_line` 做弱匹配
4. 若全部失败，标记为 `stale` 或 `missing`

## 13.3 状态定义

- `exact`：精确定位成功
- `relocated`：通过上下文重定位成功
- `stale`：文件仍存在，但原锚点失配
- `missing`：文件或上下文均不可恢复

## 14. CLI 设计

## 14.1 命令结构

```text
diffo [review-target] [-- pathspec...]
diffo comments list [--file <path>] [--json]
diffo comments get <comment-id> [--json]
diffo review status [--file <path>] [--json]
diffo themes list
diffo themes validate <file>
```

## 14.2 输出约定

### 文本输出

- 面向人工阅读，简洁清晰

### JSON 输出

- 面向脚本与 Agent
- 字段稳定、可版本化
- 推荐顶层包含 `schema_version`

示例：

```json
{
  "schema_version": 1,
  "repository_id": "repo_xxx",
  "comments": [
    {
      "comment_id": "cmt_01",
      "file_path": "src/main.zig",
      "start_line": 42,
      "end_line": 45,
      "body": "这里建议拆分函数。",
      "match_status": "exact"
    }
  ]
}
```

## 15. 主题系统设计

## 15.1 主题抽象

内部不直接绑定某个具体主题名称，而定义语义色槽：

```text
ThemeTokens
- bg_default
- bg_panel
- bg_selected
- fg_default
- fg_muted
- fg_accent
- diff_add_bg
- diff_add_fg
- diff_del_bg
- diff_del_fg
- diff_context_fg
- border
- warning
- error
- comment_badge
- reviewed_badge
- unreviewed_badge
- syntax_keyword
- syntax_string
- syntax_comment
- syntax_type
- syntax_function
- syntax_number
- syntax_operator
- syntax_plain
```

## 15.2 Catppuccin 默认主题

默认内置 Catppuccin Latte / Frappé / Macchiato / Mocha 中至少一个预设，建议默认 `Mocha` 或根据终端背景推断。

## 15.3 Base16/Base24 主题系统兼容

### 设计方式

- 解析符合 Base16/Base24 规范的输入主题
- 将其映射到内部 `ThemeTokens`
- 对缺失色槽采用派生算法补齐

### 派生规则建议

- 基础背景：`base00`
- 面板背景：`base01`
- 默认前景：`base05`
- 边框：`base03`
- 强调色：`base0D`
- 增加色：`base0B`
- 删除色：`base08`
- 警告色：`base09` 或 `base0A`

Base24 额外色位可用于评论、reviewed、selection 等扩展槽。Base16/Base24 是主题规范兼容层，不是 diffo 的默认主题。

## 15.4 代码 token 色彩映射

语法高亮 token 应映射到内部 `ThemeTokens`，避免 highlighter 直接依赖 Catppuccin、Base16 或 Base24 的原始色位。

建议映射：

- `keyword` -> `syntax_keyword`
- `string` -> `syntax_string`
- `comment` -> `syntax_comment`
- `type` -> `syntax_type`
- `function` -> `syntax_function`
- `number` -> `syntax_number`
- `operator` -> `syntax_operator`
- `plain` -> `syntax_plain`

## 15.5 终端色彩降级

支持：

- TrueColor：完整 RGB
- 256 色：近似映射
- 16 色：进一步降级

通过能力检测决定最终输出策略。

## 16. 并发与性能设计

## 16.1 加载策略

- 启动时一次性加载当前 snapshot 所需元数据
- 大 patch 内容按文件增量解码或懒渲染
- 评论与 review state 在启动时全部读取到内存
- Tree-sitter grammar 注册表与 highlight query 按需加载

## 16.2 渲染优化

- 仅重绘脏区域
- 光标移动不重复解析 diff
- split 视图提前构建逻辑对齐缓存
- Tree-sitter parse tree 按 `(file_path, side, source_hash, language)` 缓存
- 语法高亮结果按 `(file_path, side, source_hash, theme_id)` 缓存
- 对超出阈值的文件采用按视口高亮或跳过高亮

## 16.3 存储优化

- 读多写少场景，采用内存索引
- 评论按 `file_path` 建索引
- review state 按 `(review_target_id, file_path)` 建索引

## 17. 错误处理设计

统一错误类型建议：

```text
AppError
- NotGitRepository
- GitUnavailable
- InvalidReviewTarget
- DiffLoadFailed
- ParseFailed
- StorageCorrupted
- StorageWriteFailed
- TerminalUnsupported
- ViewportTooSmall
```

要求：

- 向用户输出易懂错误
- 向日志输出诊断细节
- 交互模式下尽量保留恢复路径

## 18. 日志与调试

建议提供：

- `--verbose`
- `--debug-git`
- `--trace-store`

日志默认写入用户级缓存目录，不干扰正常 TUI 输出。

## 19. 测试策略

## 19.1 单元测试

覆盖模块：

- review target 解析
- diff parser
- patch fingerprint 生成
- comment anchor 重定位
- theme mapping
- language detection
- Tree-sitter grammar registry
- highlight query mapping
- diff line to syntax token mapping
- JSON 序列化/反序列化

## 19.2 集成测试

基于临时 Git 仓库验证：

- unstaged + staged 合并视图
- commit/range/branch diff
- rename/delete/binary 文件
- reviewed 状态自动失效
- 评论读取 JSON 输出

## 19.3 快照测试

对 inline / split 渲染结果做终端快照测试，保证样式与布局稳定。

## 20. 扩展点设计

预留以下接口：

- `GitBackend`：未来可切换 Git CLI / libgit2 / mock backend
- `ThemeProvider`：支持内置主题与外部主题文件
- `CommentExporter`：未来导出到 GitHub/GitLab 评论格式
- `AgentAdapter`：面向 Agent 的更高阶结构化接口

## 21. 里程碑建议

### M1：基础可用

- CLI 参数解析
- Git diff 加载
- inline 渲染
- 文件树
- reviewed 状态持久化

### M2：完整交互

- split 渲染
- 评论系统
- JSON 查询接口
- 默认 Catppuccin 主题与 Base16/Base24 主题规范兼容层
- Tree-sitter 代码语法高亮

### M3：稳定化

- 评论重定位
- 性能优化
- 测试体系与数据迁移

## 22. 技术选型建议

## 22.1 Zig 版本

- 建议固定 Zig 稳定版本，并在仓库中锁定工具链说明

## 22.2 终端库

若不完全自行处理 ANSI/输入事件，建议选用一个足够轻量、可跨平台的终端抽象层；若生态不满足，则自行封装最小终端适配接口，避免业务层直接依赖第三方库细节。

## 22.3 JSON 方案

- V1 使用 Zig 标准库 JSON 能力即可
- 注意 schema version 与向后兼容

## 23. 总结

该架构以“Git 原生命令 + 结构化 diff 模型 + TUI 渲染 + 本地结构化存储”为核心。它在 V1 阶段实现成本可控，能够满足默认工作区 review、commit/range review、评论、已审阅状态与 Agent 可读接口等核心需求；同时通过清晰的模块边界，为后续引入更复杂的评论状态机、远端平台集成与主题扩展提供稳定基础。
