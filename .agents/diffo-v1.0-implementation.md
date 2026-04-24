# diffo v1.0 Polish UI Implementation Plan

## 1. 文档信息

- 项目名称：diffo
- 版本：v1.0 polish ui
- 文档类型：实现方案
- 目标实现语言：Zig 0.16
- 需求来源：`.agents/diffo-v1.0-requiements.md`
- 适用范围：TUI 渲染、折叠、变更跳转、帮助信息、样式统一、相关测试

## 2. 实现目标

v1.0 polish 的实现目标是把当前 TUI 从“直接渲染 flat diff 行”的模型升级为“结构化 view model + viewport renderer”的模型。

核心目标：

1. 默认 stacked view 具备现代 code review 的单列阅读体验。
2. 默认折叠多余未变更代码，并支持交互展开/收起。
3. 支持 next/previous change 跳转。
4. split view 使用左右配对行模型。
5. TUI 视觉风格统一使用 `ThemeTokens`。
6. TUI help、CLI help、README 快捷键说明保持一致。
7. 保持现有 reviewed、comment、mouse、non-TTY 静态输出能力不回归。

## 3. 当前代码改造判断

当前 `src/tui.zig` 的问题不是某个单点样式问题，而是渲染模型过早绑定到了 `DiffFile.lineCount()` 与 `diff.findLineByFlatIndex()`。

需要避免继续扩展以下模式：

- 用 flat index 同时表示原始 diff 行、可见行、光标行和可评论行。
- 在 `renderFlatLine` 中同时处理语法高亮、行号、split 对齐、选区、状态符号和颜色。
- 每次按键后从原始 diff 直接渲染屏幕，没有中间 view model。

v1.0 polish 应引入明确的中间层：

`DiffFile` -> `FileViewModel` -> `VisualRow[]` -> viewport render

其中 `DiffFile` 仍是 domain model，不应为了 TUI 折叠和布局而污染 `src/diff.zig`。

## 4. 文件与模块规划

建议保持改动集中，但不要让 `src/tui.zig` 继续膨胀。

### 4.1 新增文件

建议新增：

- `src/tui_view.zig`
  - 负责构建 `FileViewModel`、`VisualRow`、`ChangeSpan`、fold 状态映射。
  - 不写 ANSI，不依赖 terminal size。
  - 单元测试主要放在这里。

- `src/tui_render.zig`
  - 负责把 `VisualRow` 渲染为 ANSI 字符串。
  - 包含 stacked/split 渲染、gutter、背景色、文件 header、fold row。
  - 依赖 `theme.zig`、`syntax.zig` 和 `tui_text.zig`。

- `src/tui_text.zig`
  - 迁移 `fitCell`、`displayWidth`、ANSI-aware 截断、padding。
  - 单元测试覆盖 CJK、ANSI reset、截断。

如果希望第一轮改动更小，也可以先在 `src/tui.zig` 内建立私有结构，完成后再拆文件。但正式 v1.0 建议完成拆分。

### 4.2 修改文件

- `src/tui.zig`
  - 保留 terminal lifecycle、event loop、mouse handling、状态调度。
  - 删除或降级 `renderFlatLine`、`renderDiffLines` 对原始 flat diff 的直接依赖。

- `src/theme.zig`
  - 保持 token 名称不变。
  - 需要确认 `Ansi.bg` 在 TUI 中被实际使用。
  - 可补充 `style` helper，但不建议扩大主题系统范围。

- `src/cli.zig`
  - 更新 CLI help 中的 interactive keys。

- `README.md`
  - 同步更新快捷键表和 stacked/split 描述。

## 5. 数据结构设计

### 5.1 TUI Session State

当前 `State` 需要从单文件 flat index 状态升级为 session 状态。

建议结构：

```zig
const ViewMode = enum {
    stacked,
    split,
};

const State = struct {
    active_file: usize = 0,
    cursor_row: usize = 0,
    scroll_row: usize = 0,
    mode: ViewMode = .stacked,
    selection_start: ?usize = null,
    help: bool = false,
    fold_states: FoldStateMap,
    view_cache: ViewCache,
    input: InputState = .{},
};
```

兼容策略：

- 内部可以暂时保留 `DiffMode.inline_view`，但用户可见文案统一为 `stacked`。
- `cursor_line` 改名为 `cursor_row`，避免和源代码行号混淆。
- `scroll` 改名为 `scroll_row`，语义更明确。

### 5.2 VisualRow

`VisualRow` 表示 TUI viewport 中的一行可见内容，不等同于原始 diff line。

```zig
const VisualRowKind = enum {
    file_header,
    hunk_header,
    fold,
    stacked_code,
    split_code,
    file_meta,
};

const VisualRow = struct {
    kind: VisualRowKind,
    hunk_index: ?usize,
    fold_id: ?FoldId,
    change_id: ?usize,
    comment_target: ?CommentTarget,
    old_lineno: ?u32,
    new_lineno: ?u32,
    line_kind: diff.DiffLineKind,
    left: ?CodeCell,
    right: ?CodeCell,
    text: []const u8,
};
```

说明：

- `stacked_code` 使用 `left` 或 `text` 表示当前单列内容。
- `split_code` 使用 `left` 和 `right`。
- `comment_target` 用于 `c` 添加评论，不让 fold/header 误创建评论。
- `change_id` 用于 `n/N` 跳转。

### 5.3 CodeCell

```zig
const CodeCell = struct {
    kind: diff.DiffLineKind,
    old_lineno: ?u32,
    new_lineno: ?u32,
    text: []const u8,
    stable_line_id: []const u8,
};
```

`CodeCell` 是从 `DiffLine` 映射来的渲染输入，避免 renderer 直接持有复杂 `DiffLine` 指针生命周期。

### 5.4 FoldId 与 FoldState

```zig
const FoldId = struct {
    file_index: usize,
    hunk_index: usize,
    ordinal: usize,
};

const FoldState = enum {
    collapsed,
    expanded,
};
```

折叠状态只需 TUI session 内持久，不写入 store。

建议 fold key 不使用 path 字符串，优先使用 `(file_index, hunk_index, ordinal)`，因为 snapshot 在一次 TUI session 中稳定，查找成本低。

### 5.5 FileViewModel

```zig
const FileViewModel = struct {
    file_index: usize,
    mode: ViewMode,
    rows: []VisualRow,
    changes: []ChangeSpan,
    additions: usize,
    deletions: usize,
};
```

缓存失效条件：

- `active_file` 变化时可懒构建新文件 view。
- `mode` 变化时重建当前文件 view。
- `fold_states` 当前文件变化时重建当前文件 view。
- snapshot 变化在进程内不会发生，因此不需要监听 Git 文件变更。

## 6. View Model 构建算法

### 6.1 总流程

构建当前文件 view：

1. 统计文件增删数量。
2. 追加 `file_header`。
3. 遍历 hunks。
4. 按 mode 调用 stacked 或 split row builder。
5. 生成 `ChangeSpan[]`。
6. 返回可渲染 rows。

伪代码：

```zig
fn buildFileView(file: diff.DiffFile, file_index: usize, mode: ViewMode, folds: FoldStateMap) !FileViewModel {
    var builder = ViewBuilder.init(...);
    try builder.appendFileHeader(file);
    for (file.hunks, 0..) |hunk, hunk_index| {
        try builder.appendHunk(file, hunk, hunk_index, mode, folds);
    }
    return builder.finish();
}
```

### 6.2 Stacked View 行生成

stacked view 保持 unified diff 的阅读顺序：

- context 行：单列展示。
- delete 行：单列展示，删除语义背景。
- add 行：单列展示，新增语义背景。
- meta 行：弱化展示。

默认折叠应用在 context 行段上。

构建步骤：

1. 对 hunk lines 标记每行是否为 change。
2. 计算每个 context 行到最近 change 的距离。
3. 距离 `<= context_radius` 的 context 行可见。
4. 连续不可见 context 行合并为 `fold` row。
5. 若 fold state 为 expanded，则输出其包含的 context rows。
6. 其它行输出为 `stacked_code`。

默认参数：

```zig
const default_context_radius: usize = 3;
const min_fold_lines: usize = 4;
```

当不可见 context 行数小于 `min_fold_lines` 时可直接显示，避免产生过多小折叠。

### 6.3 Fold Placeholder

fold row 需要包含：

- `fold_id`
- folded line count
- expanded/collapsed 状态
- first old/new line number
- last old/new line number

展示文本：

- collapsed：`▸ 11 unmodified lines`
- expanded：`▾ 11 unmodified lines`

注意：当前开发规范默认 ASCII，但项目现有需求明确参考 caret 图标；如果担心终端兼容，可用 ASCII fallback：

- truecolor/UTF-8 环境：`▸` / `▾`
- fallback：`>` / `v`

### 6.4 Split View 配对算法

split view 不应再逐 diff 行渲染，应按 segment 配对。

处理 hunk lines：

1. context segment：每行生成左右相同内容。
2. delete segment 后紧跟 add segment：作为 modify group。
3. 单独 delete segment：左侧有内容，右侧 empty placeholder。
4. 单独 add segment：左侧 empty placeholder，右侧有内容。
5. meta segment：生成弱化 meta row。

modify group 配对：

```zig
const pair_count = @max(delete_lines.len, add_lines.len);
for (0..pair_count) |i| {
    left = if (i < delete_lines.len) delete_lines[i] else null;
    right = if (i < add_lines.len) add_lines[i] else null;
    appendSplitRow(left, right);
}
```

split 下的 fold 仍然针对 context segment 生效。fold 展开时 context 行左右两侧相同展示。

## 7. 变更跳转设计

### 7.1 ChangeSpan 生成

`ChangeSpan` 表示一个可跳转单位。

```zig
const ChangeSpan = struct {
    id: usize,
    start_row: usize,
    end_row: usize,
    hunk_index: usize,
    old_start: ?u32,
    old_end: ?u32,
    new_start: ?u32,
    new_end: ?u32,
};
```

stacked view：

- 连续 add/delete 行属于同一个 change span。
- context 行切断 change span。
- meta 行不作为 change，除非文件没有 hunk 且只有 binary/meta 信息。

split view：

- modify group 的 paired rows 属于同一个 change span。
- 单独 add/delete segment 是一个 change span。

### 7.2 `n/N` 行为

`n`：

- 找到第一个 `span.start_row > cursor_row` 的 span。
- 将 `cursor_row` 设置为 `span.start_row`。
- 调用 `ensureCursorVisibleRows`。

`N`：

- 找到最后一个 `span.start_row < cursor_row` 的 span。
- 将 `cursor_row` 设置为 `span.start_row`。

边界：

- 到最后一个 change 后继续按 `n` 不循环。
- 到第一个 change 前继续按 `N` 不循环。
- 可以设置 transient status，例如 `last change`，但不作为 P0 必须。

折叠处理：

- 默认 change 不应被折叠，因为折叠只折 context。
- 若未来 fold 允许折叠 change group，跳转前需展开目标 fold；v1.0 不做 change fold。

## 8. 输入处理方案

### 8.1 快捷键表

P0 快捷键：

| Key | Action |
| --- | --- |
| `j` / `↓` | 光标下移 |
| `k` / `↑` | 光标上移 |
| `PageDown` | 下滚一页 |
| `PageUp` | 上滚一页 |
| `J` | 下一个文件 |
| `K` | 上一个文件 |
| `n` | 下一个 change |
| `N` | 上一个 change |
| `z` | toggle 当前 fold |
| `Z` | toggle 当前文件全部 fold |
| `v` | stacked/split 切换 |
| `r` | toggle reviewed |
| `c` | 添加评论 |
| `V` | 开始选择 |
| `u` | 第一个 unreviewed 文件 |
| `?` | help |
| `q` | quit |

建议 v1.0 移除用户文档中的 `[f` / `]f`，保留兼容实现但不宣传。主路径使用 `J/K`。

### 8.2 InputState

当前 `readEvent()` 假设一次 `read` 得到完整 key sequence，简单但不稳定。

P0 可保持当前实现，新增单键 `n/N/z/Z` 不受影响。

P1 建议引入：

```zig
const InputState = struct {
    pending_prefix: ?u8 = null,
};
```

用于兼容 `[f` / `]f`：

- 读到 `[` 或 `]` 后记录 prefix。
- 下一次读到 `f` 再触发文件切换。
- 读到其它字符则清空 prefix 并处理当前字符。

Escape sequences 继续按完整 bytes 匹配方向键与 PageUp/PageDown。

## 9. 渲染方案

### 9.1 顶层布局

当前 layout 可继续使用：

- 第一行：status/file summary。
- 中间区域：main diff + optional file tree。
- 最后一行：footer/help。

建议调整：

- `appendStatus` 显示仓库、target、reviewed 进度、mode。
- stacked view 的文件 header 放在 diff content 第一行，不挤在全局 status。
- footer 正常态显示 compact help，例如 `? help  n/N change  z fold  v split  r reviewed  q quit`。
- help 态显示多行 overlay 或至少两行分组；如果仍只允许一行，必须包含 P0 快捷键。

### 9.2 Stacked Row 样式

推荐列布局：

```text
<bar><line-no>  <code>
```

建议宽度：

- `bar`: 1 cell
- gap: 1 cell
- `line-no`: dynamic width，至少 4，按当前文件最大 line number 计算
- gap: 2 cells
- code: remaining width

样式：

- delete：bar 红色，背景 `diff_del_bg`，前景 `diff_del_fg`
- add：bar 绿色，背景 `diff_add_bg`，前景 `diff_add_fg`
- context：bar 空，背景默认，前景 `diff_context_fg`
- selected/current：使用 `bg_selected` 或在语义背景上使用更亮的 variant。当前 token 没有 variant，P0 可优先使用 `bg_selected` 覆盖背景，保留 bar 颜色。

### 9.3 Fold Row 样式

```text
<caret>  11 unmodified lines
```

实现：

- 整行 `bg_panel`
- caret 区域固定 3 cells
- 文本 `fg_muted`
- 鼠标点击 fold row 可作为 P1；P0 先支持键盘 `z`

### 9.4 File Header Row

文件 header 展示：

```text
utils.ts -> code_utils.ts                                      -7 +4
```

实现：

- 左侧：`old_path -> path` 或 `path`
- 右侧：`-{deletions} +{additions}`
- 使用 `fg_default`、`diff_del_fg`、`diff_add_fg`
- 背景默认或 `bg_panel`

### 9.5 Split Row 样式

split view 列布局：

```text
<left-line-no>  <left-code> │ <right-line-no>  <right-code>
```

注意：

- 左右 code width 需要扣除行号、gap 和 separator。
- 空侧使用斜线占位可以作为 P1，P0 可先留空。
- 当前行高亮必须覆盖左右两侧整行。

### 9.6 ANSI 与截断

必须把以下能力集中到 `tui_text.zig`：

- visible width 计算。
- ANSI-aware 截断。
- padding。
- 每行末尾 reset。

`fitCell` 现有逻辑可迁移，但需要补强：

- 支持非 `m` 结尾的 CSI 忽略或安全跳过。
- 截断后主动 append `ansi.reset()`。
- 保证背景色不会泄漏到下一行。

## 10. Mouse 行为调整

当前 mouse 基于 `state.scroll + mouse.y - body_y` 计算 flat line。迁移后改为 visual row：

- 点击 diff 区：`cursor_row = scroll_row + relative_y`
- 若点击 fold row：P1 可 toggle fold；P0 可只移动 cursor，用户再按 `z`
- 滚轮 diff 区：移动 `scroll_row`，并让 cursor 跟随或保持可见
- 点击 file tree：保持现有行为
- 滚轮 file tree：保持现有行为

注意：

- 当前 macOS J/K panic 已修复为 `fileTreeStartIndex`，新实现不要恢复类似 unsigned 下溢表达式。
- 所有 row index 操作通过 helper 完成，不直接写 `a - b + c`。

## 11. 评论与 reviewed 兼容

### 11.1 评论

`c` 添加评论需要从 `cursor_row` 映射回 `DiffLine`。

实现方式：

- `VisualRow.comment_target` 保存 file/hunk/line 信息。
- fold/header/file_meta 行没有 `comment_target`，按 `c` 不创建评论。
- split row 有 left/right 双侧时，优先选择当前 cursor side。P0 没有 side cursor，可优先选择 right/add 行；若只有 left/delete 行则选择 left。

### 11.2 Selection

当前 selection 用 row index 范围实现。迁移后仍用 visual row 范围。

注意：

- selection 跨 fold row 时，添加评论只对可评论 rows 生效。
- P0 可保持原行为：选区只影响 `end_line`，不引入多评论。

### 11.3 Reviewed

`r` 不依赖 row model，保持现有逻辑：

```zig
store.setReviewed(repo_id, target_id, file, reviewed)
```

文件切换、fold、view mode 不应影响 reviewed 状态。

## 12. 实施阶段

### Phase 0：准备与安全修复

目标：为大改做稳定地基。

任务：

- 保留并提交 macOS J/K unsigned underflow 修复。
- 同步需求文档。
- 确认 `just check` 通过。

验收：

- `zig build test`
- `zig build`
- `just check`

### Phase 1：抽出 text/render 基础工具

目标：降低后续渲染复杂度。

任务：

- 新增 `src/tui_text.zig`。
- 迁移 `fitCell`、`displayWidth`、`utf8SeqLen`。
- 增加 ANSI 截断和 reset 测试。
- 保持 UI 输出基本不变。

风险：

- ANSI visible width 计算影响所有 TUI 行。

验收：

- 现有 TUI 可启动。
- CJK 和 ANSI 单元测试通过。

### Phase 2：引入 FileViewModel

目标：建立 view model，但先不改变视觉效果。

任务：

- 新增 `src/tui_view.zig`。
- 实现 `VisualRow`、`FileViewModel`。
- stacked mode 先按当前 flat diff 生成 rows。
- 光标和滚动从 flat line 切到 visual row。
- `c` 从 visual row 映射回原 diff line。

验收：

- 行移动、滚动、文件切换、评论、reviewed 不回归。

### Phase 3：默认 context folding

目标：实现默认折叠和 `z/Z`。

任务：

- 实现 context radius 折叠算法。
- 实现 `FoldStateMap`。
- 实现 `z` toggle 当前 fold。
- 实现 `Z` toggle 当前文件全部 fold。
- 更新 mouse click 到 visual row。

测试：

- 长 context 折叠。
- 小 context 不折叠。
- toggle 后 row 数正确。
- 光标不会落到越界 row。

### Phase 4：Change navigation

目标：实现 `n/N`。

任务：

- 生成 `ChangeSpan[]`。
- 实现 next/previous change。
- 更新 footer/help。

测试：

- 单 hunk 多 change。
- 多 hunk change。
- 文件头/fold/header 不被当成 change。
- 到边界不循环、不 panic。

### Phase 5：Stacked view 样式

目标：实现参考图中的默认单列体验。

任务：

- 文件 header row。
- add/delete 背景色和 gutter 竖条。
- fold row panel 样式。
- line number gutter 固定宽度。
- 当前行/选区视觉统一。
- 使用 `ThemeTokens` 的 bg token。

验收：

- add/delete/context/fold/header 视觉层次明确。
- 长行截断不破坏 ANSI reset。
- 非 truecolor 退化可读。

### Phase 6：Split pairing

目标：把 split view 改为左右配对模型。

任务：

- 实现 segment parser。
- 实现 delete/add modify group pairing。
- 实现 split row renderer。
- 保持 fold 和 change navigation 在 split 下可用。

测试：

- delete/add 数量相等。
- delete 多于 add。
- add 多于 delete。
- 只有 add。
- 只有 delete。
- context folded/expanded。

### Phase 7：Help、README、CLI 同步

目标：消除文档和实际行为不一致。

任务：

- 更新 TUI footer help。
- 更新 CLI `helpText`。
- 更新 README 快捷键表。
- 决定 `[f` / `]f` 是否保留宣传。建议不宣传，仅保留兼容。

验收：

- 三处快捷键说明一致。
- 新增 `n/N/z/Z` 都被记录。

## 13. 测试计划

### 13.1 单元测试

新增测试文件重点放在 `tui_view.zig` 与 `tui_text.zig`。

必须覆盖：

- fold generation
- fold toggle
- change span generation
- next/previous change lookup
- split pairing
- ANSI-aware width/truncation
- file tree start underflow regression

### 13.2 集成测试

使用现有 `zig build test`。

若后续测试成本可接受，可新增 fixture patch 字符串：

- `fixtures/large-context.patch`
- `fixtures/split-pairing.patch`
- `fixtures/rename-modify.patch`

但 P0 不强制引入 fixture 文件，可先用 Zig multiline string。

### 13.3 手工验证清单

至少手工跑：

```sh
just check
zig build run
zig build run -- --cached
zig build run -- HEAD~1..HEAD
```

在 TUI 中验证：

- `J/K`
- `j/k`
- mouse wheel
- mouse click file tree
- `z/Z`
- `n/N`
- `v`
- `r`
- `c`
- `?`
- `q`

终端覆盖：

- WSL Windows Terminal
- macOS Terminal 或 iTerm2
- tmux with `set -g mouse on`
- non-TTY：`zig-out/bin/diffo > /tmp/diffo.txt`

## 14. 风险与应对

### 14.1 Row index 迁移风险

风险：旧逻辑使用 flat diff line，迁移后评论和 selection 可能指错行。

应对：

- 所有可评论 row 必须保存 `CommentTarget`。
- `c` 不再通过 row index 反查原始 `DiffFile`。

### 14.2 ANSI 背景泄漏

风险：背景色未 reset 会污染整屏。

应对：

- renderer 每行统一 append reset。
- `fitCell` 截断后也 reset。
- 增加单元测试检查截断 ANSI string 以 reset 结尾。

### 14.3 性能回退

风险：每帧重建全文件 view 和高亮导致大 diff 卡顿。

应对：

- P0 可接受当前文件 view 每次状态变化重建。
- P1 增加 per-file cache。
- 高亮只对 viewport rows 执行，view model 不存高亮字符串。

### 14.4 Unicode 兼容

风险：参考样式中的 `▸`、`▾`、竖条在部分终端宽度不一致。

应对：

- 使用 display width helper。
- 提供 ASCII fallback。
- 不依赖双宽符号进行布局。

### 14.5 Split view 复杂度

风险：split pairing 和 fold 同时上线容易引入 bug。

应对：

- 先完成 stacked + fold + change navigation。
- split pairing 放到 Phase 6，建立在同一 view model 上。

## 15. 建议提交拆分

建议按以下 commit 拆分，便于 review：

1. `Fix file tree scroll underflow`
2. `Add v1.0 polish requirements and implementation docs`
3. `Extract TUI text utilities`
4. `Add TUI view model`
5. `Add context folding controls`
6. `Add change navigation`
7. `Polish stacked diff rendering`
8. `Implement paired split rendering`
9. `Sync interactive help docs`

## 16. 完成定义

v1.0 polish 完成时必须满足：

- `just check` 通过。
- TUI 默认进入 stacked view。
- 大 context 默认折叠。
- `z/Z` 可用。
- `n/N` 可用。
- add/delete 背景和 gutter 可见。
- split view 左右配对可用。
- TUI help、CLI help、README 快捷键一致。
- macOS、WSL、tmux 基础交互不 panic。
- reviewed/comment/untracked/non-TTY 行为不回归。

