# diffo v1.0 Polish UI Requirements

## 1. 文档信息

- 项目名称：diffo
- 版本：v1.0 polish ui
- 文档类型：补充需求文档
- 目标实现语言：Zig 0.16
- 参考来源：
  - `.agents/requirements-design.md`
  - `.agents/architecture-design.md`
  - 当前代码 review
  - 用户提供的 UI 参考图

## 2. 背景与目标

diffo v1 已实现本地 Git diff review 的基础能力，包括默认工作区 diff、untracked 文件展示、文件级 reviewed 状态、评论、基础 TUI、鼠标点击与滚轮支持。

本轮 v1.0 polish 的目标是将当前“可用的 diff 文本视图”升级为“适合日常代码审阅的 TUI 体验”，重点改善代码可读性、折叠策略、变更跳转、帮助信息正确性和整体视觉一致性。其中 stacked view 是默认单列代码审阅视图，样式以用户补充截图为主要参考。

本需求不改变 diffo 的核心产品定位：仍然是运行在终端中的本地 Git diff review 工具，不引入远端平台 PR review、复杂评论线程或 Web UI。

## 3. 现状 Review 摘要

### 3.1 当前可复用能力

- `src/diff.zig` 已具备结构化 diff 模型：`DiffFile`、`DiffHunk`、`DiffLine`。
- `src/tui.zig` 已具备基础状态：当前文件、当前行、滚动位置、inline/split 模式、选择起点、help 开关。
- `src/theme.zig` 已定义统一主题 token，包括 diff add/delete 背景色、前景色、panel、border、selection 等语义颜色。
- TUI 已支持 SGR mouse 事件，包含滚轮和点击。

### 3.2 主要问题与优化机会

- 当前 split view 是逐 diff 行左右分栏，不会把删除行和新增行配对成同一视觉行；修改块在左右对照时不够直观。
- `ThemeTokens` 中的 `diff_add_bg`、`diff_del_bg`、`bg_panel`、`bg_selected` 等背景色 token 基本未参与 TUI 渲染，导致 UI 风格不统一。
- 当前 TUI 未实现未变更代码折叠，长文件 review 时噪音过多。
- 当前没有 next/previous change 快捷键，只能逐行移动或切换文件。
- 当前 TUI footer help 与 README/实际按键不完全一致，容易误导用户。
- `[f` / `]f` 这类多字符快捷键在 raw terminal 单字节读取下可能不稳定，需要改为明确的输入状态机或使用单键替代。
- 当前 diff 渲染没有独立的可测试视图模型，折叠、变更跳转和 split 对齐若直接堆在 `renderFlatLine` 上会继续增加复杂度。

## 4. 范围

### 4.1 本轮必须完成

1. 美化代码 diff 渲染，整体风格参考用户提供图片。
2. 默认折叠多余未变更代码。
3. 添加 toggle 折叠快捷键。
4. 新增/删除/修改渲染需更接近现代 code review 体验。
5. 统一 TUI 颜色、边框、状态栏、文件列表、footer help 的视觉语言。
6. 添加跳转 next/previous change 的快捷键。
7. 修复 TUI help 中错误或缺失的快捷键信息。
8. 保持现有鼠标与滚轮能力可用。
9. 增加必要测试，避免折叠和跳转逻辑回归。

### 4.2 本轮不做

- 不实现远端 GitHub/GitLab PR review。
- 不实现评论线程、resolved/unresolved 状态机。
- 不引入非终端 UI。
- 不强制集成 Tree-sitter；现有 lexical fallback 可继续作为 v1.0 默认语法高亮。
- 不要求实现主题选择器 UI，但渲染层必须使用已有语义 token，便于后续主题扩展。

## 5. 用户体验需求

### 5.1 默认视图

进入 TUI 后，默认展示当前文件的 diff 内容。

默认渲染策略：

- 变更块必须展开。
- 每个变更块前后保留少量上下文行，默认建议为 3 行。
- 超出上下文窗口的连续未变更代码默认折叠。
- 折叠块显示为一行 compact placeholder，例如：
  - `▸ 24 unchanged lines`
  - `▾ 24 unchanged lines`
- 折叠块需要在 stacked 和 split 模式下都能正确显示。

### 5.2 折叠交互

必须支持快捷键 toggle 当前折叠块。

建议快捷键：

- `z`：toggle 当前光标所在折叠块。
- `Z`：toggle 当前文件全部折叠块。

行为要求：

- 光标在折叠块上按 `z`，展开该折叠块。
- 光标在已展开的可折叠上下文区域内按 `z`，收起对应折叠块。
- 展开后展示完整未变更代码，并保留原始行号。
- 收起后光标应停留在折叠块 placeholder 上，不应跳到无效行。
- 文件切换后保留每个文件的折叠状态，至少在当前 TUI session 内保留。

### 5.3 变更跳转

必须支持在当前文件内跳转到上一个/下一个 change。

建议快捷键：

- `n`：next change。
- `N`：previous change。

change 定义：

- add 行。
- delete 行。
- 修改块中由 delete/add 配对组成的视觉行。
- binary、rename-only、mode-only 文件可将文件元信息视为一个 change。

行为要求：

- 跳转目标应落到 change 的第一条可见行。
- 若目标 change 位于折叠区域内，系统必须自动展开必要折叠块或跳到对应折叠块后展开。
- 到达文件末尾后不循环，除非后续单独增加配置。
- footer 或 status 可短暂提示 `last change` / `first change`，但不是本轮强制项。

### 5.4 Split View 渲染

split view 应从“逐 diff 行分栏”调整为“左右对照行模型”。

要求：

- 删除行显示在左侧，新增行显示在右侧。
- 连续 delete 后紧跟连续 add 的修改块应尽量按行配对。
- delete 多于 add 时，右侧显示空位或斜线占位。
- add 多于 delete 时，左侧显示空位或斜线占位。
- context 行左右两侧都显示。
- line number 左右分别展示 old/new 行号。
- 左右面板之间保留清晰分隔线。
- 当前光标行需有统一高亮，不应只改变某一侧文本颜色。

### 5.5 Stacked View 渲染

stacked view 是默认单列 diff 视图，用于快速阅读一个文件内按时间顺序堆叠的删除、增加和上下文代码。当前代码中的 `inline_view` 可以作为内部枚举名保留，但用户可见文案建议统一为 `stacked`。

要求：

- 文件头使用紧凑 header，展示当前文件路径；rename 场景展示 `old_path -> new_path`。
- 文件头右侧展示当前文件增删统计，例如 `-7 +4`。
- 折叠行使用整行 panel 背景，左侧有可点击/可聚焦的 caret 区域，文本格式参考 `11 unmodified lines`。
- add 行使用新增背景与前景色，并在最左侧 gutter 使用绿色竖条强化变更范围。
- delete 行使用删除背景与前景色，并在最左侧 gutter 使用红色竖条强化变更范围。
- context 行使用默认背景与 muted/context 前景色。
- hunk header 可隐藏或弱化；若显示，应使用 panel 背景或 muted 风格，不应打断代码阅读。
- 删除行和新增行按原 diff 顺序堆叠显示：先 delete，后 add；修改块内不做左右分栏。
- line number gutter 固定宽度，删除行显示 old line number，新增行显示 new line number；修改块中 old/new 行号颜色应能区分。
- 代码内容起始列必须稳定对齐，不因行号位数、状态符号或 ANSI 颜色变化产生抖动。
- 当前光标行应使用统一选中态，不能破坏 add/delete 的语义背景；可通过叠加 gutter 或更亮背景实现。
- 当前光标行、选区行、评论标记不能互相覆盖。
- 长行截断时不破坏 ANSI reset，也不让光标高亮泄漏到下一行。

### 5.6 文件列表

右侧文件树需要和主 diff 区风格统一。

要求：

- 当前文件有明显选中态。
- reviewed/unreviewed 状态使用统一 icon 或短标记。
- 文件状态 `A/M/D/R/C/B` 保留。
- 评论数量保留。
- 文件路径过长时稳定截断，不造成布局跳动。
- 鼠标点击文件仍可切换。
- 鼠标滚轮在文件树区域仍可移动文件。

### 5.7 Help

TUI help 必须准确反映实际快捷键。

至少包含：

- `j/k` 或 `↑/↓`：移动光标。
- `PageUp/PageDown`：大步滚动。
- `J/K`：切换文件。
- `n/N`：next/previous change。
- `z/Z`：折叠当前/全部折叠块。
- `v`：切换 inline/split。
- `r`：切换 reviewed。
- `c`：添加评论。
- `V`：开始选择。
- `u`：跳到第一个 unreviewed 文件。
- `?`：显示/隐藏 help。
- `q`：退出。

若保留 `[f` / `]f`，必须保证输入解析可靠，并在 help 中展示；否则应从 help 和 README 中移除，避免错误说明。

## 6. 技术需求

### 6.1 渲染模型重构

应在 `src/tui.zig` 内拆出独立的 diff view model，避免继续把折叠、split 对齐、光标、语法高亮全部堆入 `renderFlatLine`。

建议新增概念：

- `VisualRow`
  - hunk header
  - fold placeholder
  - stacked code row
  - split paired row
  - file meta row
  - file header row
- `ChangeSpan`
  - start visual row
  - end visual row
  - hunk index
  - old/new line range
- `FoldState`
  - file path 或 file index
  - hunk index
  - fold id
  - expanded/collapsed

### 6.2 折叠算法

输入为 `DiffFile.hunks`，输出为可渲染 `VisualRow` 序列。

建议策略：

1. 对每个 hunk 内的 `DiffLine` 扫描 change 行。
2. 对连续 context 行按距离最近 change 的位置分组。
3. change 前后 `context_radius=3` 的 context 行默认可见。
4. 其它连续 context 行合并为 fold placeholder。
5. 文件开头/结尾长 context 也应折叠。

### 6.3 Split 配对算法

对每个 hunk 建议按段处理：

- context 段：生成左右相同的 paired row。
- delete/add 相邻段：生成 modification group，按最大长度配对。
- 只有 delete：左侧有内容，右侧空占位。
- 只有 add：右侧有内容，左侧空占位。
- meta 行：生成跨列或左右空占位提示。

### 6.4 输入解析

需要减少多字节和多字符快捷键的不稳定性。

要求：

- Escape sequence 继续支持方向键、PageUp、PageDown。
- 如果保留 `[f` / `]f`，输入层必须支持 prefix buffer，而不是假设一次 read 能得到两个字节。
- `n/N/z/Z` 等新增快捷键必须为单键操作，降低 raw terminal 输入复杂度。

### 6.5 颜色与样式

必须开始使用 `theme.ThemeTokens` 中已有语义色。

要求：

- add/delete 行使用对应 bg + fg。
- 当前行使用 `bg_selected`。
- hunk/fold/header 使用 `bg_panel`、`fg_muted`、`border`。
- 文件树选中态使用 `bg_selected`。
- ANSI reset 必须在每行结束前写入。
- 非 truecolor 终端下应优雅退化，不输出破碎控制码。

### 6.6 性能

目标：

- 5000 行 diff 内，常规移动、滚动、折叠切换不应有明显卡顿。
- 每帧只渲染当前 viewport 所需文本，避免无意义构建全文件高亮字符串。
- 折叠和 change index 可在文件切换时懒构建并缓存到 session state。

## 7. 验收标准

### 7.1 功能验收

- 打开包含大量 context 的 diff 时，多余 context 默认折叠。
- 按 `z` 可以展开/收起当前折叠块。
- 按 `Z` 可以展开/收起当前文件全部折叠块。
- 按 `n` 跳到下一个 change。
- 按 `N` 跳到上一个 change。
- stacked view 中 add/delete 行具有明显背景色与左侧变更竖条。
- split view 中 delete/add 修改块左右对齐。
- 文件列表选中态、review 状态、评论数显示稳定。
- help 内容与实际快捷键一致。
- 鼠标滚轮和点击能力不回归。

### 7.2 回归验收

- `zig build test` 通过。
- `zig build` 通过。
- `just check` 通过。
- 非 TTY 输出仍能渲染静态 diff，不进入 alternate screen。
- 无变更时仍输出 `diffo: no changes for this review target`。
- untracked 文件仍包含在默认 review 目标中。
- `r` reviewed 状态持久化能力不回归。
- `c` 添加评论能力不回归。

## 8. 测试建议

### 8.1 单元测试

新增测试覆盖：

- fold row 生成：长 context 被折叠，短 context 不折叠。
- fold toggle：展开/收起后 visual row 数量正确。
- change index：连续 add/delete/context 的 next/previous change 位置正确。
- split pairing：delete/add 数量相等、不等、只有 add、只有 delete 的配对结果正确。
- input parser：`[f` / `]f` 若保留，需覆盖分多次 read 的情况。

### 8.2 手工测试样例

准备至少 5 类 diff：

- 小型单文件修改。
- 大文件少量修改。
- 新增文件。
- 删除文件。
- rename + modify。
- untracked 文件。
- binary 文件。

### 8.3 终端环境测试

至少覆盖：

- Windows Terminal + WSL。
- tmux 开启 mouse。
- 普通 Linux terminal。
- 非 TTY 输出重定向。
- truecolor 与非 truecolor 环境。

## 9. 实现优先级

### P0

- 修复 help 内容错误。
- 增加 next/previous change 快捷键。
- 默认折叠多余 context。
- toggle 折叠快捷键。
- 保证现有 TUI 功能不回归。

### P1

- split view delete/add 配对渲染。
- add/delete 背景色渲染。
- 文件树选中态和整体 UI 风格统一。
- 输入解析状态机。

### P2

- 更丰富的 fold placeholder 样式。
- 状态栏短暂提示。
- 更细粒度的语法高亮。
- 后续主题切换入口。

## 10. 代码 Review 发现的具体修复点

- `src/tui.zig` 当前 footer help 只展示一行简略帮助，缺少方向键、PageUp/PageDown、`[f`/`]f`、新增折叠和 change 跳转说明，需要重写。
- `src/cli.zig` CLI help 的 interactive keys 也不完整，TUI help 修复时应同步更新，避免 README、CLI help、TUI help 三处不一致。
- `src/tui.zig` 当前 `renderFlatLine` 同时处理行号、颜色、split、选择和语法高亮，后续需求会让该函数过重，建议先拆 view model 再做复杂渲染。
- `src/tui.zig` 当前 split view 没有修改块配对算法，无法达到参考图中的左右对照效果。
- `src/theme.zig` 已有背景色 token，但 TUI 渲染未充分使用，应补齐 `Ansi.bg` 的使用路径。
- `src/tui.zig` 当前多字符快捷键依赖单次 read 得到完整字节序列，`[f`/`]f` 可能不稳定，建议改为单键主路径或输入状态机。

## 11. 交付物

- 更新后的 TUI 渲染代码。
- 更新后的快捷键处理代码。
- 更新后的 README 快捷键说明。
- 更新后的 CLI help。
- 新增或更新的测试。
- 通过 `just check` 的验证记录。
