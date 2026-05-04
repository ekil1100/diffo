# Tree-sitter Syntax Highlighting Implementation Plan

## 1. 文档信息

- 项目名称：diffo
- 文档类型：实现方案
- 目标实现语言：Zig 0.16.0
- 适用范围：语法高亮、TUI 渲染、构建系统、主题映射、测试
- 当前状态：第一版已接入 vendored Tree-sitter runtime 和 Zig grammar；Zig 文件优先使用 Tree-sitter highlight query，其他语言和不可用 source side 回退到 lexical fallback

## 2. 背景与目标

diffo 当前通过 `syntax.highlightLine(...)` 对单行文本做轻量词法高亮。这个方案实现简单、适合逐行渲染，但对复杂语法支持有限，例如多行字符串、嵌套注释、模板字符串、泛型、宏、类型上下文、函数调用等。

本方案选择引入 Tree-sitter，目标是把语法高亮升级为“文件级解析 + 行级渲染”的模型，同时保留现有 fallback，确保 Tree-sitter 不成为 diffo 运行的硬前提。

核心目标：

1. 支持基于 Tree-sitter AST 与 highlight query 的高质量语法高亮。
2. diff 语义高亮仍然优先：add/delete/context/selected 的背景色由 TUI 控制。
3. 对不支持的语言、解析失败、大文件、无颜色终端自动回退到当前 lexical fallback。
4. 支持 stacked 和 split view。
5. 为后续“函数范围折叠、符号跳转、只看当前函数 diff”等结构化能力保留边界。

非目标：

1. 不替换 Git 原生 diff 行为。
2. 不在第一阶段实现所有语言。
3. 不把 Tree-sitter query 暴露为完整用户配置系统。
4. 不要求删除现有 lexical fallback。
5. 不在 TUI 滚动时重复解析文件。

## 3. 总体设计

当前调用链：

```text
DiffFile / DiffLine
  -> tui.renderStackedCodeRow / tui.renderSplitCell
  -> syntax.highlightLine(language, line.text)
  -> ANSI string
  -> tui_text.fitCell
  -> styleCell + row background
```

目标调用链：

```text
DiffSnapshot
  -> SyntaxSession.build(snapshot)
  -> per-file SideHighlighter(old/new)
  -> HighlightedLine spans
  -> tui renderer asks highlighter for line text + syntax spans
  -> styleCell applies row background after ANSI resets
```

关键变化：

- `syntax.highlightLine` 从主路径降级为 fallback。
- Tree-sitter 以文件为单位解析，而不是逐行解析。
- 高亮结果以 token span 存储，再由 TUI 渲染成 ANSI。
- 删除行走 old side 内容，新增行和上下文行优先走 new side 内容。

## 4. 模块规划

建议保持第一轮改动集中，避免一次性大拆目录。

### 4.1 新增文件

- `src/tree_sitter.zig`
  - Tree-sitter C ABI 声明。
  - `Parser`、`Tree`、`Language`、`Query`、`QueryCursor` 的轻量 Zig wrapper。
  - 管理 `ts_parser_new/delete`、`ts_tree_delete`、`ts_query_delete` 等生命周期。

- `src/syntax_cache.zig`
  - 文件级高亮缓存。
  - 保存每个文件 old/new side 的解析树和行级 token spans。
  - 对 TUI 暴露行级查询 API。

- `src/syntax_query.zig`
  - 加载内置 highlight query。
  - 将 Tree-sitter capture 名映射到 `theme.ThemeTokens` 的语义 token。

- `src/syntax_grammars.zig`
  - 语言 registry。
  - 根据 `DiffFile.language` 返回 Tree-sitter language function 和 query 文本。

### 4.2 修改文件

- `build.zig`
  - 编译 vendored Tree-sitter runtime。
  - 编译选定 grammar 的 `parser.c` 和可选 `scanner.c` / `scanner.cc`。
  - 对 C++ scanner 语言增加 libc++/libstdc++ 链接策略，第一阶段尽量避开需要 C++ scanner 的 grammar。

- `src/syntax.zig`
  - 保留 lexical fallback。
  - 增加高亮 token 类型和公共渲染 helper。
  - `registryStatus()` 返回 Tree-sitter 可用状态、已注册语言数量、fallback 状态。

- `src/tui.zig`
  - 在 TUI session 初始化时创建 `SyntaxCache`。
  - 渲染代码行时优先向 `SyntaxCache` 查询高亮结果。
  - 查询失败再调用现有 `syntax.highlightLine(...)`。

- `src/git.zig`
  - 若现有 snapshot 只保存 patch，需要补充读取 old/new side 完整文件内容的能力。
  - 提供 source-side loader，能按 section 9.1 的矩阵读取 HEAD blob、index blob `:<path>` 和 working tree file。
  - explicit commit/range target 再使用 `git show <rev>:<path>` 读取对应 revision 的 blob。

- `src/diff.zig`
  - 尽量不污染核心 diff model。
  - 可只保留 `DiffLine.old_lineno/new_lineno/language` 作为映射输入。

## 5. Vendored 依赖布局

建议把 Tree-sitter 相关源代码 vendored 到仓库中，避免用户构建时再拉取依赖。

推荐目录：

```text
vendor/
  tree-sitter/
    lib/
      include/tree_sitter/api.h
      src/lib.c
  tree-sitter-zig/
    src/parser.c
    queries/highlights.scm
  tree-sitter-javascript/
    src/parser.c
    src/scanner.c
    queries/highlights.scm
```

第一阶段建议只接一个语言：

- 首选：`tree-sitter-zig`
- 备选：`tree-sitter-javascript`

选择 Zig 的理由：

- 项目自身就是 Zig，方便用仓库内代码验证。
- 能直接提升 diffo 自己开发时的体验。
- 语言检测中已经对 `.zig` 和 `build.zig` 做了识别。

注意事项：

- vendored grammar 需要记录来源、commit、license。
- 每次升级 grammar 都要跑高亮快照测试。
- query 文件应从 grammar 仓库随版本固定，不要动态下载。

## 6. Build 接入方案

`build.zig` 已经使用 `link_libc = true`，因此可以直接编译 Tree-sitter C runtime。

Tree-sitter C runtime、grammar C sources 和 include paths 必须加到所有会引用 `src/tree_sitter.zig` 的 compile artifact 上。当前项目至少包括：

- `exe`
- `mod_tests`
- `exe_tests`

如果只加到 `exe`，`zig build test` 会在 test artifact 中因为缺少 include path 或未链接 `ts_*` / grammar symbols 而失败。

推荐增加 helper：

```zig
fn addTreeSitterSources(b: *std.Build, compile: *std.Build.Step.Compile) void {
    compile.addIncludePath(b.path("vendor/tree-sitter/lib/include"));
    compile.addCSourceFile(.{
        .file = b.path("vendor/tree-sitter/lib/src/lib.c"),
        .flags = &.{ "-std=c11" },
    });
}

fn addGrammar(
    b: *std.Build,
    compile: *std.Build.Step.Compile,
    path: []const u8,
) void {
    compile.addIncludePath(b.path(path ++ "/src"));
    compile.addCSourceFile(.{
        .file = b.path(path ++ "/src/parser.c"),
        .flags = &.{ "-std=c11" },
    });
}
```

实际 Zig build API 对 comptime 字符串拼接有限，落地时可写成显式调用，并对 `exe`、`mod_tests`、`exe_tests` 都调用同一个 helper：

```zig
fn addSyntaxNativeSources(b: *std.Build, compile: *std.Build.Step.Compile) void {
    compile.addIncludePath(b.path("vendor/tree-sitter/lib/include"));
    compile.addCSourceFile(.{ .file = b.path("vendor/tree-sitter/lib/src/lib.c"), .flags = &.{ "-std=c11" } });
    compile.addCSourceFile(.{ .file = b.path("vendor/tree-sitter-zig/src/parser.c"), .flags = &.{ "-std=c11" } });
}

addSyntaxNativeSources(b, exe);
addSyntaxNativeSources(b, mod_tests);
addSyntaxNativeSources(b, exe_tests);
```

如果 grammar 有 `scanner.c`：

```zig
compile.addCSourceFile(.{ .file = b.path("vendor/tree-sitter-javascript/src/scanner.c"), .flags = &.{ "-std=c11" } });
```

这个调用也应放在 `addSyntaxNativeSources(...)` 这类 shared helper 内，确保 `exe`、`mod_tests`、`exe_tests` 一致。

如果 grammar 有 `scanner.cc`，第一阶段建议先不接，避免引入 C++ 链接复杂度。后续需要时再加：

- macOS：链接 libc++。
- Linux：链接 libstdc++ 或 libc++。
- Windows：需要单独验证。

## 7. Zig C ABI 边界

`src/tree_sitter.zig` 不应暴露过多 C 细节给 TUI。

推荐结构：

```zig
const std = @import("std");

const c = @cImport({
    @cInclude("tree_sitter/api.h");
});

pub const Language = *const c.TSLanguage;

pub const Parser = struct {
    raw: *c.TSParser,

    pub fn init(language: Language) !Parser {}
    pub fn deinit(self: *Parser) void {}
    pub fn parse(self: Parser, source: []const u8) !Tree {}
};

pub const Tree = struct {
    raw: *c.TSTree,

    pub fn deinit(self: *Tree) void {}
};

pub const Query = struct {
    raw: *c.TSQuery,

    pub fn init(language: Language, source: []const u8) !Query {}
    pub fn deinit(self: *Query) void {}
};
```

原则：

- C pointer 生命周期只在 wrapper 内管理。
- 对外返回 diffo 自己的 `HighlightSpan`，不要让 TUI 持有 Tree-sitter node。
- 所有 C 错误统一转成 Zig error，例如 `error.TreeSitterQueryInvalid`、`error.TreeSitterParseFailed`。

## 8. 高亮数据结构

### 8.1 Token 类型

Tree-sitter capture 名很多，diffo 主题 token 只有有限集合。建议把 capture 映射到内部 token：

```zig
pub const SyntaxToken = enum {
    keyword,
    string,
    comment,
    type,
    function,
    number,
    operator,
    plain,
};
```

映射示例：

```text
keyword, keyword.*          -> keyword
string, string.*            -> string
comment, comment.*          -> comment
type, type.*, constructor   -> type
function, function.*, method -> function
number, constant.numeric    -> number
operator, punctuation.*     -> operator
```

无法识别的 capture 默认忽略，而不是乱上色。

### 8.2 Span

```zig
pub const HighlightSpan = struct {
    start_byte: usize,
    end_byte: usize,
    token: SyntaxToken,
};
```

span 坐标使用当前行内 byte offset，方便 renderer 拼接。

### 8.3 行级高亮结果

```zig
pub const HighlightedLine = struct {
    text: []const u8,
    spans: []const HighlightSpan,
};
```

`text` 指向 side source 中的行切片或缓存副本。由于 diff line 已经有 `line.text`，第一阶段也可以只返回 spans，并继续使用 diff line text。

### 8.4 文件 side

```zig
pub const DiffSide = enum {
    old,
    new,
};

const SideHighlight = struct {
    source: []u8,
    line_starts: []usize,
    spans_by_line: []const []const HighlightSpan,
};
```

`spans_by_line` 使用 1-based 源文件行号映射时，数组 0 可空置，减少 old/new line number 转换成本。

## 9. 文件内容获取策略

Tree-sitter 需要完整源码。diff patch 中的行不够准确，尤其是多行语法。

### 9.1 Working tree / index 模式

不同 diff source 的 old/new side 不能用同一条读取规则概括，必须按 Git 实际比较对象选择源码。

文件 side 来源必须和 Git diff 语义一致：

```text
source       old side              new side
unstaged     index :<path>          working tree file
staged       HEAD:<path>            index :<path>
untracked    none                   working tree file
```

说明：

- unstaged diff 比较的是 index 和 working tree，因此 old side 必须读 `:<path>`，不能读 `HEAD:<path>`。
- staged diff 比较的是 HEAD 和 index，因此 new side 必须读 `:<path>`。
- untracked file 没有 old side，delete 行不应存在。
- 如果 `:<path>` 在 index 中不存在，按 missing side 处理并 fallback。

### 9.2 Explicit target

对于 `HEAD~1..HEAD`、`main...feature` 等 explicit target，需要从 normalized target 推导 base/head revision。

第一阶段可以简化：

- 对 explicit target 只对 diff line text 做 lexical fallback。
- 或只解析 new side，当能可靠推导 head revision 时才启用 Tree-sitter。

正式支持 explicit target 时，需要在 `ReviewTarget` 中保存解析后的 left/right revision：

```zig
const TreeSitterSourceSpec = struct {
    old_rev: ?[]const u8,
    new_rev: ?[]const u8,
};
```

如果不想扩大 `diff.zig`，可以在 `git.zig` 内部提供：

```zig
pub fn loadFileSide(
    allocator: std.mem.Allocator,
    repo: diff.Repository,
    target: diff.ReviewTarget,
    file: diff.DiffFile,
    side: syntax_cache.DiffSide,
) !?[]u8
```

### 9.3 删除、重命名、二进制文件

- deleted file：只解析 old side。
- renamed file：old side 使用 `old_path`，new side 使用 `path`。
- binary file：跳过 Tree-sitter。
- missing file：fallback，不报 fatal。

## 10. Query 执行与 Span 生成

Tree-sitter highlight query 输出的是 capture 对应的 node range。裸 `TSQueryCursor` 只负责枚举 pattern match 和 capture；query predicates/directives 需要 host 侧显式处理，不能直接把所有 capture 上色。

必须支持的 predicate/directive 策略：

- 第一阶段若使用完整 grammar 自带 `highlights.scm`，实现 `#eq?`、`#not-eq?`、`#match?`、`#not-match?`、`#any-of?` 等常见 predicate 过滤。
- 对无法识别的 predicate/directive，保守丢弃该 pattern 的 captures，而不是上色。
- 对 injection、local scope、combined capture 等高阶 directive，第一阶段可以忽略，但必须保证忽略不会把本应过滤的 capture 上色。
- 如果不实现 predicate evaluator，Phase 1 必须使用经过审计的最小 query subset，并在测试中确认没有 predicate/directive。

实现步骤：

1. 对完整 source 调用 parser，得到 `TSTree`。
2. 用 language 对应的 `highlights.scm` 创建 `TSQuery`。
3. 用 `TSQueryCursor` 遍历 capture。
4. 对每个 pattern match 读取并执行 query predicates/directives。
5. 只保留 predicate 通过且 capture 名可映射的 captures。
6. 取 node start/end byte 与 start/end point。
7. 将跨行 capture 拆成多行 span。
8. 同一范围多个 capture 冲突时按优先级处理。

优先级建议：

```text
comment > string > keyword > function > type > number > operator > plain
```

跨行拆分规则：

- start line：`start_byte` 到该行结尾。
- 中间行：整行。
- end line：行开始到 `end_byte`。

span 合并规则：

- 相邻且 token 相同的 span 可以合并。
- 重叠 span 先按优先级裁剪，第一阶段也可以采用“后写高优先级覆盖低优先级”。

## 11. TUI 渲染集成

当前 TUI 已有关键保护：

- `tui_text.fitCell` 能处理 ANSI escape，不把颜色码算入宽度。
- `styleCell` 会在 ANSI reset 后重涂背景和基础前景。
- `rowBg` 和 `rowFg` 保持 diff 语义。

Tree-sitter 渲染时必须遵守：

1. 只输出前景色 ANSI，不输出背景色。
2. 每个 token 后可以 reset，但 TUI 必须重涂背景。
3. 无颜色终端直接返回纯文本。
4. 裁剪发生在 ANSI 化之后，继续使用 `tui_text.fitCell`。

建议新增：

```zig
pub fn renderHighlightedLine(
    allocator: std.mem.Allocator,
    ansi: theme.Ansi,
    tokens: theme.ThemeTokens,
    text: []const u8,
    spans: []const HighlightSpan,
) ![]u8
```

渲染逻辑：

1. 维护 `cursor` byte offset。
2. 先追加 plain 文本。
3. 对 span 区间追加 token fg code + text + reset。
4. 对 invalid UTF-8 或越界 span 直接跳过该 span。

TUI 使用策略：

```zig
const highlighted = syntax_cache.highlightDiffLine(
    allocator,
    ansi,
    palette,
    file,
    line,
) catch try syntax.highlightLine(allocator, ansi, palette, file.language, line.text);
```

## 12. Diff 行到 Tree-sitter 行的映射

`DiffLine` 已有：

- `old_lineno`
- `new_lineno`
- `kind`
- `text`

映射规则：

```text
delete  -> old side + old_lineno
add     -> new side + new_lineno
context -> new side + new_lineno，如果没有 new_lineno 则 old side + old_lineno
meta    -> 不做语法高亮
```

对于 split view：

- left cell 使用 left `DiffLine` 自己的 kind 和 line number。
- right cell 使用 right `DiffLine` 自己的 kind 和 line number。

如果从 side source 取出的行文本和 `DiffLine.text` 不一致：

- 第一阶段仍以 `DiffLine.text` 渲染，Tree-sitter span 只在范围可信时应用。
- 如果 byte 长度明显不匹配，跳过 Tree-sitter span，fallback lexical。

原因：

- diff 行可能经过 Git 转义、缺少换行标记、或处于 rename/copy 特殊状态。
- TUI 展示必须以 Git diff 为准。

## 13. 缓存策略

### 13.1 Session 内缓存

TUI session 内 snapshot 不变，因此可以按文件懒构建：

```zig
const FileHighlightKey = struct {
    file_index: usize,
    side: DiffSide,
};
```

缓存失效条件：

- 进程退出。
- snapshot 重新加载。
- theme 改变。
- color mode 改变。

当前没有 runtime theme switching，因此第一阶段只需 session 内缓存。

### 13.2 磁盘缓存

第一阶段不建议实现磁盘缓存。理由：

- 需要考虑 grammar version、query version、theme version、terminal color mode。
- diff review session 通常短，session cache 已足够。

后续如需磁盘缓存，key 可以包含：

```text
repo_id
review_target_id
file path
side
blob sha or patch_fingerprint
language
grammar version
query version
theme name
true_color flag
```

## 14. 配置与开关

建议新增环境变量和后续 config 字段：

```text
DIFFO_SYNTAX=auto|tree-sitter|fallback|off
```

语义：

- `auto`：默认。能用 Tree-sitter 就用，否则 fallback。
- `tree-sitter`：强制尝试 Tree-sitter，失败时仍 fallback，但 footer 显示失败原因。
- `fallback`：只用 lexical fallback。
- `off`：关闭语法高亮，只保留 diff 语义颜色。

第一阶段可以只实现内部 config，不暴露 CLI 参数。

`registryStatus()` 输出建议：

```text
tree_sitter: 1 grammar loaded, fallback enabled
lexical_fallback: tree-sitter disabled or unavailable
disabled: syntax highlighting disabled
```

## 15. 测试计划

### 15.1 Unit tests

`src/tree_sitter.zig`

- parser init/deinit 不泄漏。
- invalid query 返回错误。
- valid Zig source 能 parse。

`src/syntax_query.zig`

- capture 名到 `SyntaxToken` 映射。
- unknown capture 被忽略。

`src/syntax_cache.zig`

- delete 行映射 old side。
- add 行映射 new side。
- context 行优先映射 new side。
- missing side fallback。
- binary file skipped。

`src/syntax.zig`

- fallback 仍然可用。
- `NO_COLOR` 下不输出 ANSI。
- ANSI reset 不破坏 text 内容。

### 15.2 TUI renderer tests

- highlighted span 渲染后 `tui_text.fitCell` 宽度正确。
- CJK + ANSI 组合裁剪正确。
- token reset 后行背景被重涂。
- split view 两侧独立高亮。

### 15.3 Golden tests

为第一阶段语言准备固定输入：

```text
tests/fixtures/syntax/zig/basic.zig
tests/fixtures/syntax/zig/basic.expected.txt
```

golden 不建议保存完整 ANSI 色码，可以保存 token spans：

```json
[
  { "line": 1, "start": 0, "end": 3, "token": "keyword" },
  { "line": 1, "start": 10, "end": 14, "token": "function" }
]
```

### 15.4 Integration tests

- `zig build test`
- 用本仓库 diff 打开 TUI，确认 footer 显示 `syntax=tree_sitter`。
- 设置 `DIFFO_SYNTAX=fallback` 后确认回退。
- 设置 `NO_COLOR=1` 后确认无 ANSI 输出。

## 16. 分阶段实施计划

### Phase 0：准备与约束确认

目标：

- 固定 Tree-sitter runtime 和第一个 grammar 的版本。
- 确认 license。
- 确认 Zig 0.16.0 能编译 vendored C 源。

交付：

- `vendor/tree-sitter`
- `vendor/tree-sitter-zig`
- `build.zig` 能编译 runtime 和 grammar。
- 不改 TUI 行为。

验证：

```sh
zig build test
```

### Phase 1：C ABI wrapper

目标：

- 完成 `src/tree_sitter.zig`。
- 能 parse 一段 Zig 源码。
- 能编译 highlight query。

交付：

- `Parser` / `Tree` / `Query` wrapper。
- 基础 tests。

验证：

```sh
zig build test --summary all
```

### Phase 2：Query 到 token spans

目标：

- 执行 `highlights.scm`。
- 生成 line-based `HighlightSpan`。
- 处理跨行 capture 和重叠 capture。

交付：

- `src/syntax_query.zig`
- `src/syntax_grammars.zig`
- token span tests。

### Phase 3：SyntaxCache 与 file side source

目标：

- 对 TUI session 按文件构建高亮缓存。
- working tree 模式下读取 old/new side。
- 不支持的 target 自动 fallback。

交付：

- `src/syntax_cache.zig`
- `git.zig` 增加 side source loader。
- delete/add/context 映射 tests。

### Phase 4：TUI 集成

目标：

- stacked/split view 使用 Tree-sitter spans。
- footer 显示实际 syntax mode。
- fallback 路径稳定。

交付：

- 修改 `src/tui.zig`。
- 保持现有行背景、评论标记、选中行、split 对齐。

验证：

- `zig build test`
- 手动打开 diffo 查看 Zig 文件 diff。

### Phase 5：多语言扩展

候选顺序：

1. Zig
2. JavaScript / TypeScript
3. Python
4. Rust
5. Go
6. C / C++
7. JSON

每加一个语言必须包含：

- grammar vendored source。
- highlights query。
- language registry entry。
- 至少一个 span golden test。

## 17. 风险与应对

### 17.1 Build 复杂度

风险：

- grammar 带 C++ scanner。
- 不同平台 C/C++ 链接差异。

应对：

- 第一阶段只接无 C++ scanner 或 scanner.c 的 grammar。
- CI 覆盖 macOS/Linux。
- Windows 保持 static output 能构建，交互 TUI 仍按现有 POSIX 边界。

### 17.2 Diff side 内容不完整

风险：

- explicit range 不容易推导 old/new source。
- 删除文件、rename、staged/unstaged 混合状态容易错。

应对：

- 第一阶段 working tree 和 staged 优先。
- 不确定时 fallback。
- 展示永远以 Git diff line text 为准。

### 17.3 性能

风险：

- 大文件 parse 卡顿。
- query 生成 spans 成本高。

应对：

- 使用 `Config.max_file_size`，默认 512 KiB。
- 懒加载当前文件。
- session 内缓存。
- 不在每次滚动时 parse。

### 17.4 ANSI 与背景冲突

风险：

- query token reset 清掉行背景。

应对：

- Tree-sitter 层只输出 fg。
- 继续使用 `styleCell` 的 reset 后重涂逻辑。
- 加 reset/background regression test。

### 17.5 Query 质量不稳定

风险：

- 不同 grammar query capture 风格不统一。

应对：

- 建立 capture 到有限 `SyntaxToken` 的映射。
- unknown capture 忽略。
- 每个语言保留最小 golden test。

## 18. 推荐第一版验收标准

第一版合格标准：

1. `zig build test` 通过。
2. Zig 文件 diff 使用 Tree-sitter 高亮。
3. unsupported language 自动 fallback。
4. `NO_COLOR=1` 下没有语法 ANSI。
5. 大文件超过阈值 fallback。
6. split view 左右两侧高亮不串色、不破坏行背景。
7. footer 能显示 Tree-sitter/fallback 状态。
8. README 中 Tree-sitter 说明从“boundary only”更新为实际支持范围。

## 19. 建议的第一批代码改动顺序

按最小风险提交：

1. vendor Tree-sitter runtime 和 Zig grammar。
2. `build.zig` 编译 runtime/grammar，增加空 smoke test。
3. 新增 `tree_sitter.zig` wrapper。
4. 新增 query capture -> token span。
5. 新增 `syntax.renderHighlightedLine(...)`。
6. 新增 session `SyntaxCache`，先只支持 new side。
7. 接入 TUI stacked view。
8. 接入 split view。
9. 补 old side 加强 delete 行高亮。
10. 更新 README 和 footer 文案。

这个顺序让每一步都能单独构建和测试，避免一次性把 build、parser、query、TUI 渲染全部绑在一起。
