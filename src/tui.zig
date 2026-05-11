const std = @import("std");
const builtin = @import("builtin");
const diff = @import("diff.zig");
const inline_diff = @import("inline_diff.zig");
const store_mod = @import("store.zig");
const syntax = @import("syntax.zig");
const syntax_cache = @import("syntax_cache.zig");
const theme = @import("theme.zig");
const tui_text = @import("tui_text.zig");
const tui_view = @import("tui_view.zig");
const util = @import("util.zig");

pub const DiffMode = tui_view.ViewMode;

const State = struct {
    active_file: usize = 0,
    cursor_row: usize = 0,
    scroll_row: usize = 0,
    mode: DiffMode = .stacked,
    selection_start: ?usize = null,
    copy_notice: CopyNotice = .none,
    help: bool = false,
    pending_g: bool = false,
    folds: std.ArrayList(tui_view.FoldEntry) = .empty,
    syntax_cache: syntax_cache.SyntaxCache,
    preserve_scroll_once: bool = false,

    fn init(allocator: std.mem.Allocator, io: std.Io, snapshot: diff.DiffSnapshot) State {
        return .{
            .syntax_cache = syntax_cache.SyntaxCache.init(allocator, io, snapshot.repository, snapshot.review_target, false),
        };
    }

    fn deinit(self: *State, allocator: std.mem.Allocator) void {
        self.folds.deinit(allocator);
        self.syntax_cache.deinit();
    }
};

const CopyNotice = union(enum) {
    none,
    copied: usize,
    empty,
    comment_added,
};

const Size = struct {
    width: usize,
    height: usize,
};

const Layout = struct {
    width: usize,
    height: usize,
    body_y: usize,
    body_height: usize,
    main_width: usize,
    sidebar_x: usize,
    sidebar_width: usize,
    footer_y: usize,

    fn init(size: Size) Layout {
        const width = @max(size.width, 60);
        const height = @max(size.height, 8);
        const sidebar_width: usize = if (width >= 120)
            @min(@as(usize, 52), @max(@as(usize, 36), width / 4))
        else if (width >= 90)
            32
        else
            0;
        const separator_width: usize = if (sidebar_width > 0) 1 else 0;
        const main_width = width - sidebar_width - separator_width;
        return .{
            .width = width,
            .height = height,
            .body_y = 2,
            .body_height = height - 2,
            .main_width = main_width,
            .sidebar_x = main_width + separator_width + 1,
            .sidebar_width = sidebar_width,
            .footer_y = height,
        };
    }
};

const Event = union(enum) {
    key: []const u8,
    mouse: MouseEvent,
};

const enter_tui_screen = "\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h";
const leave_tui_screen = "\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l";
const suspend_tui_input = "\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?25h";
const resume_tui_input = "\x1b[?1000h\x1b[?1002h\x1b[?1006h\x1b[?25l";
const enable_extended_keyboard = "\x1b[>1u\x1b[>4;2m";
const disable_extended_keyboard = "\x1b[<u\x1b[>4;0m";

const MouseEvent = struct {
    button: u16,
    x: usize,
    y: usize,
    pressed: bool,

    fn isWheelUp(self: MouseEvent) bool {
        return self.button == 64;
    }

    fn isWheelDown(self: MouseEvent) bool {
        return self.button == 65;
    }

    fn isLeftPress(self: MouseEvent) bool {
        return self.button == 0 and self.pressed;
    }

    fn isLeftDrag(self: MouseEvent) bool {
        return self.pressed and (self.button & 32) != 0 and (self.button & 3) == 0;
    }
};

pub fn run(
    allocator: std.mem.Allocator,
    io: std.Io,
    snapshot: *diff.DiffSnapshot,
    store: *store_mod.Store,
    author: []const u8,
) !void {
    if (snapshot.files.len == 0) {
        try writeAll(io, "diffo: no changes for this review target\n");
        return;
    }

    var state = State.init(allocator, io, snapshot.*);
    defer state.deinit(allocator);
    if (builtin.os.tag == .windows) {
        const screen = try render(allocator, snapshot.*, store.*, &state, false);
        defer allocator.free(screen);
        try writeAll(io, screen);
        return;
    }

    const is_tty = (std.Io.File.stdout().isTty(io) catch false) and (std.Io.File.stdin().isTty(io) catch false);
    if (!is_tty) {
        const screen = try render(allocator, snapshot.*, store.*, &state, false);
        defer allocator.free(screen);
        try writeAll(io, screen);
        return;
    }

    var raw = RawTerminal.enter() catch null;
    defer if (raw) |*term| term.restore();

    var signals = SignalCleanup.install();
    defer signals.restore();

    try writeAll(io, enter_tui_screen);
    SignalCleanup.active = true;
    defer {
        writeAll(io, leave_tui_screen) catch {};
        SignalCleanup.active = false;
    }

    while (true) {
        const screen = try render(allocator, snapshot.*, store.*, &state, true);
        defer allocator.free(screen);
        try writeAll(io, screen);

        const event = readEvent() orelse continue;
        if (try handleEvent(allocator, io, snapshot, store, author, &state, event)) break;
    }
}

fn handleEvent(
    allocator: std.mem.Allocator,
    io: std.Io,
    snapshot: *diff.DiffSnapshot,
    store: *store_mod.Store,
    author: []const u8,
    state: *State,
    event: Event,
) !bool {
    switch (event) {
        .mouse => |mouse| {
            state.pending_g = false;
            state.copy_notice = .none;
            try handleMouse(allocator, snapshot.*, state, mouse);
            return false;
        },
        .key => |key| {
            if (key.len == 0) return false;
            if (key[0] == 3) return true;
            if (key[0] == 'q') return true;
            if (isPlainEscape(key)) {
                cancelSelectionMode(state);
                return false;
            }
            if (state.help and key[0] != '?') {
                state.help = false;
                state.pending_g = false;
                state.copy_notice = .none;
                return false;
            }
            if (key[0] != 'y') state.copy_notice = .none;

            switch (gotoAction(key, state.pending_g)) {
                .none => state.pending_g = false,
                .pending => {
                    state.pending_g = true;
                    return false;
                },
                .top => {
                    state.pending_g = false;
                    try jumpToEdge(allocator, snapshot.*, state, .top);
                    return false;
                },
                .bottom => {
                    state.pending_g = false;
                    try jumpToEdge(allocator, snapshot.*, state, .bottom);
                    return false;
                },
            }

            if (std.mem.eql(u8, key, "\x1b[A")) {
                try moveLine(allocator, snapshot.*, state, -1);
                return false;
            }
            if (std.mem.eql(u8, key, "\x1b[B")) {
                try moveLine(allocator, snapshot.*, state, 1);
                return false;
            }
            if (std.mem.eql(u8, key, "\x1b[5~")) {
                try scrollLines(allocator, snapshot.*, state, -12);
                return false;
            }
            if (std.mem.eql(u8, key, "\x1b[6~")) {
                try scrollLines(allocator, snapshot.*, state, 12);
                return false;
            }

            switch (key[0]) {
                'j' => try moveLine(allocator, snapshot.*, state, 1),
                'k' => try moveLine(allocator, snapshot.*, state, -1),
                'J' => moveFile(snapshot.*, state, 1),
                'K' => moveFile(snapshot.*, state, -1),
                'n' => try jumpChange(allocator, snapshot.*, state, 1),
                'N' => try jumpChange(allocator, snapshot.*, state, -1),
                'z' => try toggleCurrentFold(allocator, snapshot.*, state),
                'Z' => try toggleAllFolds(allocator, snapshot.*, state),
                'v' => {
                    state.mode = if (state.mode == .stacked) .split else .stacked;
                    clampCursor(allocator, snapshot.*, state) catch {};
                },
                '?' => state.help = !state.help,
                'V' => state.selection_start = state.cursor_row,
                'y' => {
                    const copied = try copySelectionToClipboard(allocator, io, snapshot.*, state);
                    state.copy_notice = if (copied == 0) .empty else .{ .copied = copied };
                },
                'u' => jumpUnreviewed(snapshot.*, store.*, state),
                'r' => {
                    const file = snapshot.files[state.active_file];
                    const reviewed = !store.isReviewed(file.path, file.patch_fingerprint, snapshot.review_target.target_id);
                    try store.setReviewed(snapshot.repository.repo_id, snapshot.review_target.target_id, file, reviewed);
                },
                'c' => {
                    try writeAll(io, suspend_tui_input);
                    var resume_after_comment = true;
                    defer if (resume_after_comment) writeAll(io, resume_tui_input) catch {};
                    const body = readCommentBody(allocator, io, theme.Ansi.init(true), theme.catppuccinMocha()) catch |err| switch (err) {
                        error.Interrupted => {
                            resume_after_comment = false;
                            return true;
                        },
                        else => return err,
                    };
                    defer if (body) |text| allocator.free(text);
                    if (body) |text| if (text.len > 0) {
                        if (try addCommentAtCursor(allocator, snapshot.*, store, state, text, author)) state.copy_notice = .comment_added;
                    };
                    state.selection_start = null;
                },
                '[' => if (key.len >= 2 and key[1] == 'f') moveFile(snapshot.*, state, -1),
                ']' => if (key.len >= 2 and key[1] == 'f') moveFile(snapshot.*, state, 1),
                else => {},
            }
            return false;
        },
    }
}

fn handleMouse(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, mouse: MouseEvent) !void {
    const layout = Layout.init(terminalSize());
    const in_sidebar = layout.sidebar_width > 0 and mouse.x >= layout.sidebar_x;
    if (mouse.isWheelUp()) {
        if (in_sidebar) moveFile(snapshot, state, -3) else try scrollLines(allocator, snapshot, state, -3);
        return;
    }
    if (mouse.isWheelDown()) {
        if (in_sidebar) moveFile(snapshot, state, 3) else try scrollLines(allocator, snapshot, state, 3);
        return;
    }
    const left_drag = mouse.isLeftDrag();
    if (!mouse.isLeftPress() and !left_drag) return;
    if (mouse.y < layout.body_y or mouse.y >= layout.footer_y) return;

    if (in_sidebar) {
        if (left_drag) return;
        if (mouse.y == layout.body_y) return;
        const start = fileTreeStart(snapshot, state, layout.body_height);
        const idx = start + mouse.y - layout.body_y - 1;
        if (idx < snapshot.files.len) {
            state.active_file = idx;
            state.cursor_row = 0;
            state.scroll_row = 0;
            state.selection_start = null;
        }
    } else {
        var view = try currentView(allocator, snapshot, state);
        defer view.deinit(allocator);
        const total = view.rows.len;
        if (total == 0) return;
        const target = try rowAtVisualOffset(allocator, snapshot, state, view, mouse.y - layout.body_y, layout);
        if (left_drag) {
            if (state.selection_start == null) state.selection_start = state.cursor_row;
        } else {
            state.selection_start = null;
        }
        state.cursor_row = target;
        try ensureCursorVisible(allocator, snapshot, view, state);
    }
}

fn render(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    store: store_mod.Store,
    state: *State,
    colors: bool,
) ![]u8 {
    const layout = Layout.init(terminalSize());
    const palette = theme.catppuccinMocha();
    const ansi = theme.Ansi.init(colors);
    const clear_eol = if (colors) "\x1b[K" else "";
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    try ensureRenderViewport(allocator, snapshot, view, state, layout);

    var out: std.ArrayList(u8) = .empty;
    if (colors) try out.appendSlice(allocator, "\x1b[?2026h\x1b[H");
    try appendStatus(allocator, &out, snapshot, store, state, layout.width);
    try out.appendSlice(allocator, clear_eol);
    try out.append(allocator, '\n');

    const diff_lines = try renderDiffLines(allocator, state, snapshot.files[state.active_file], state.active_file, store, view, layout.main_width, layout.body_height, ansi, palette);
    defer {
        for (diff_lines) |line| allocator.free(line);
        allocator.free(diff_lines);
    }
    const tree_lines = try renderFileTree(allocator, snapshot, store, state, layout.sidebar_width, layout.body_height, ansi, palette);
    defer {
        for (tree_lines) |line| allocator.free(line);
        allocator.free(tree_lines);
    }

    for (0..layout.body_height) |i| {
        const left = if (i < diff_lines.len) diff_lines[i] else "";
        try appendRenderedCell(allocator, &out, left, layout.main_width);
        if (layout.sidebar_width > 0) {
            try out.appendSlice(allocator, "│");
            const right = if (i < tree_lines.len) tree_lines[i] else "";
            try appendRenderedCell(allocator, &out, right, layout.sidebar_width);
        }
        try out.appendSlice(allocator, clear_eol);
        try out.append(allocator, '\n');
    }

    if (state.help) {
        try tui_text.appendCell(allocator, &out, "j/k arrows move  G/gg bottom/top  PgUp/PgDn scroll  J/K file  n/N change  z/Z fold  v view  r reviewed  c comment  V select  y copy  Esc clear  u unreviewed  ? help  q quit", layout.width);
    } else {
        const active_file = snapshot.files[state.active_file];
        const syntax_mode = activeSyntaxMode(snapshot, active_file);
        const footer = switch (state.copy_notice) {
            .none => try std.fmt.allocPrint(allocator, "mode={s} target={s} syntax={s}  ? help  V select  y copy  n/N change  z fold  q quit", .{ state.mode.label(), snapshot.review_target.normalized_spec, @tagName(syntax_mode) }),
            .empty => try std.fmt.allocPrint(allocator, "nothing copyable at cursor  mode={s} target={s} syntax={s}  V select  y copy", .{ state.mode.label(), snapshot.review_target.normalized_spec, @tagName(syntax_mode) }),
            .copied => |line_count| try std.fmt.allocPrint(allocator, "copied {d} line{s} to clipboard  mode={s} target={s} syntax={s}  V select  y copy", .{ line_count, if (line_count == 1) "" else "s", state.mode.label(), snapshot.review_target.normalized_spec, @tagName(syntax_mode) }),
            .comment_added => try std.fmt.allocPrint(allocator, "comment added  mode={s} target={s} syntax={s}  c comment  q quit", .{ state.mode.label(), snapshot.review_target.normalized_spec, @tagName(syntax_mode) }),
        };
        defer allocator.free(footer);
        try tui_text.appendCell(allocator, &out, footer, layout.width);
    }
    if (colors) try out.appendSlice(allocator, "\x1b[K\x1b[?2026l");
    return out.toOwnedSlice(allocator);
}

fn appendStatus(allocator: std.mem.Allocator, out: *std.ArrayList(u8), snapshot: diff.DiffSnapshot, store: store_mod.Store, state: *const State, width: usize) !void {
    var reviewed: usize = 0;
    for (snapshot.files) |file| {
        if (store.isReviewed(file.path, file.patch_fingerprint, snapshot.review_target.target_id)) reviewed += 1;
    }
    const file = snapshot.files[state.active_file];
    const line = try std.fmt.allocPrint(allocator, " diffo  {s}  {s}  files {d}/{d} reviewed  file {d}/{d}  {s}", .{ snapshot.repository.current_branch, snapshot.review_target.normalized_spec, reviewed, snapshot.files.len, state.active_file + 1, snapshot.files.len, file.path });
    defer allocator.free(line);
    try tui_text.appendCell(allocator, out, line, width);
}

fn activeSyntaxMode(snapshot: diff.DiffSnapshot, file: diff.DiffFile) syntax.HighlightMode {
    if (file.is_binary or snapshot.review_target.kind != .working_tree) return .disabled;
    return syntax.modeForLanguage(file.language);
}

fn appendRenderedCell(allocator: std.mem.Allocator, out: *std.ArrayList(u8), text: []const u8, width: usize) !void {
    if (text.len == 0) {
        for (0..width) |_| try out.append(allocator, ' ');
        return;
    }
    try out.appendSlice(allocator, text);
}

fn renderDiffLines(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    store: store_mod.Store,
    view: tui_view.FileView,
    width: usize,
    height: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![][]u8 {
    var lines: std.ArrayList([]u8) = .empty;
    errdefer {
        for (lines.items) |line| allocator.free(line);
        lines.deinit(allocator);
    }
    const total = view.rows.len;
    const start = @min(state.scroll_row, total);
    const end = @min(total, start + height);
    const line_width = lineNumberWidth(file);
    for (start..end) |row_index| {
        if (lines.items.len >= height) break;
        const rendered = try renderVisualRowLines(allocator, state, file, file_index, store, view, view.rows[row_index], row_index, width, line_width, height - lines.items.len, ansi, palette);
        defer allocator.free(rendered);
        for (rendered) |line| try lines.append(allocator, line);
    }
    return lines.toOwnedSlice(allocator);
}

fn renderVisualRowLines(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    store: store_mod.Store,
    view: tui_view.FileView,
    row: tui_view.VisualRow,
    row_index: usize,
    width: usize,
    line_width: usize,
    max_lines: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![][]u8 {
    const selected = isSelected(state, row_index);
    if (max_lines == 0) return allocator.alloc([]u8, 0);
    switch (row.kind) {
        .file_header => return oneRenderedLine(allocator, try renderFileHeader(allocator, file, view, width, ansi, palette)),
        .hunk_header => return oneRenderedLine(allocator, try renderPanelRow(allocator, row.hunk_header orelse "", width, selected, ansi, palette)),
        .fold => return oneRenderedLine(allocator, try renderFoldRow(allocator, row, width, selected, ansi, palette)),
        .stacked_code, .file_meta => {
            const line = row.line orelse return oneRenderedLine(allocator, try renderPanelRow(allocator, "", width, selected, ansi, palette));
            return renderStackedCodeRows(allocator, state, file, file_index, store, line, width, line_width, selected, max_lines, ansi, palette);
        },
        .split_code => return renderSplitCodeRows(allocator, state, file, file_index, store, row, width, line_width, selected, max_lines, ansi, palette),
    }
}

fn oneRenderedLine(allocator: std.mem.Allocator, line: []u8) ![][]u8 {
    errdefer allocator.free(line);
    const lines = try allocator.alloc([]u8, 1);
    lines[0] = line;
    return lines;
}

fn renderFileHeader(
    allocator: std.mem.Allocator,
    file: diff.DiffFile,
    view: tui_view.FileView,
    width: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    const path = if (file.old_path) |old|
        if (util.eql(old, file.path)) try util.dupe(allocator, file.path) else try std.fmt.allocPrint(allocator, "{s} -> {s}", .{ old, file.path })
    else
        try util.dupe(allocator, file.path);
    defer allocator.free(path);
    const stats = try fileStats(allocator, view.deletions, view.additions);
    defer allocator.free(stats);
    const prefix_width: usize = 2;
    const right_padding: usize = 1;
    const gap_width = if (width > prefix_width + path.len + stats.len + right_padding) width - prefix_width - path.len - stats.len - right_padding else 1;
    var raw: std.ArrayList(u8) = .empty;
    defer raw.deinit(allocator);
    try raw.appendSlice(allocator, "  ");
    try raw.appendSlice(allocator, path);
    for (0..gap_width) |_| try raw.append(allocator, ' ');
    try raw.appendSlice(allocator, stats);
    for (0..right_padding) |_| try raw.append(allocator, ' ');
    return styleCell(allocator, raw.items, width, ansi, palette.bg_panel, palette.fg_default);
}

fn fileStats(allocator: std.mem.Allocator, deletions: usize, additions: usize) ![]u8 {
    if (deletions == 0 and additions == 0) return util.dupe(allocator, "");
    if (deletions == 0) return std.fmt.allocPrint(allocator, "+{d}", .{additions});
    if (additions == 0) return std.fmt.allocPrint(allocator, "-{d}", .{deletions});
    return std.fmt.allocPrint(allocator, "-{d} +{d}", .{ deletions, additions });
}

fn renderPanelRow(
    allocator: std.mem.Allocator,
    text: []const u8,
    width: usize,
    selected: bool,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    return styleCell(allocator, text, width, ansi, if (selected) palette.bg_selected else palette.bg_panel, palette.fg_muted);
}

fn renderFoldRow(
    allocator: std.mem.Allocator,
    row: tui_view.VisualRow,
    width: usize,
    selected: bool,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    const caret = if (row.fold_expanded) "v" else ">";
    const text = try std.fmt.allocPrint(allocator, " {s}  {d} unmodified lines", .{ caret, row.fold_line_count });
    defer allocator.free(text);
    return styleCell(allocator, text, width, ansi, if (selected) palette.bg_selected else palette.bg_panel, palette.fg_muted);
}

fn renderStackedCodeRow(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    store: store_mod.Store,
    line: *const diff.DiffLine,
    width: usize,
    line_width: usize,
    selected: bool,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    const rows = try renderStackedCodeRows(allocator, state, file, file_index, store, line, width, line_width, selected, 1, ansi, palette);
    defer allocator.free(rows);
    return rows[0];
}

fn renderStackedCodeRows(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    store: store_mod.Store,
    line: *const diff.DiffLine,
    width: usize,
    line_width: usize,
    selected: bool,
    max_lines: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![][]u8 {
    const label = try lineLabel(allocator, line, line_width);
    defer allocator.free(label);
    const highlighted = try renderCodeText(allocator, state, file, file_index, line, ansi, palette);
    defer allocator.free(highlighted);
    const bar = switch (line.kind) {
        .add => "|",
        .delete => "|",
        else => " ",
    };
    const comment_mark = if (lineHasComment(store, file, line)) "!" else " ";
    const prefix = try std.fmt.allocPrint(allocator, "{s}{s} {s}  ", .{ bar, comment_mark, label });
    defer allocator.free(prefix);
    const wrap_widths = lineWrapWidths(width, prefix.len, line.text);
    const continuation_prefix = try blankLabel(allocator, wrap_widths.continuation_prefix);
    defer allocator.free(continuation_prefix);
    const wrapped = try wrapAnsiTextWithWidths(allocator, highlighted, wrap_widths.first, wrap_widths.continuation, max_lines);
    defer {
        for (wrapped) |segment| allocator.free(segment);
        allocator.free(wrapped);
    }
    var rows: std.ArrayList([]u8) = .empty;
    errdefer {
        for (rows.items) |row| allocator.free(row);
        rows.deinit(allocator);
    }
    for (wrapped, 0..) |segment, i| {
        const raw = try std.fmt.allocPrint(allocator, "{s}{s}", .{ if (i == 0) prefix else continuation_prefix, segment });
        defer allocator.free(raw);
        try rows.append(allocator, try styleCell(allocator, raw, width, ansi, rowBg(selected, line.kind, palette), rowFg(line.kind, palette)));
    }
    return rows.toOwnedSlice(allocator);
}

fn renderCodeText(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    line: *const diff.DiffLine,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    return state.syntax_cache.highlightDiffLine(allocator, ansi, palette, file_index, file, line) catch |err| switch (err) {
        error.SyntaxUnavailable, error.SourceTooLarge => try util.dupe(allocator, line.text),
        else => return err,
    };
}

fn continuationPrefixWidth(row_width: usize, prefix_width: usize, code_indent_width: usize) usize {
    if (row_width <= prefix_width + 1) return prefix_width;
    return @min(row_width - 1, prefix_width + code_indent_width + 1);
}

const WrapWidths = struct {
    first: usize,
    continuation: usize,
    continuation_prefix: usize,
};

fn lineWrapWidths(row_width: usize, prefix_width: usize, text: []const u8) WrapWidths {
    const continuation_prefix = continuationPrefixWidth(row_width, prefix_width, codeIndentWidth(text));
    return .{
        .first = if (row_width > prefix_width) row_width - prefix_width else 1,
        .continuation = if (row_width > continuation_prefix) row_width - continuation_prefix else 1,
        .continuation_prefix = continuation_prefix,
    };
}

fn codeIndentWidth(text: []const u8) usize {
    var width: usize = 0;
    var i: usize = 0;
    while (i < text.len) {
        if (text[i] == ' ') {
            width += 1;
            i += 1;
            continue;
        }
        if (text[i] == '\t') {
            width += 4;
            i += 1;
            continue;
        }
        break;
    }
    return width;
}

fn wrapAnsiText(allocator: std.mem.Allocator, text: []const u8, width: usize, max_lines: usize) ![][]u8 {
    return wrapAnsiTextWithWidths(allocator, text, width, width, max_lines);
}

fn wrapAnsiTextWithWidths(
    allocator: std.mem.Allocator,
    text: []const u8,
    first_width: usize,
    continuation_width: usize,
    max_lines: usize,
) ![][]u8 {
    if (max_lines == 0) return allocator.alloc([]u8, 0);
    var lines: std.ArrayList([]u8) = .empty;
    errdefer {
        for (lines.items) |line| allocator.free(line);
        lines.deinit(allocator);
    }
    var current: std.ArrayList(u8) = .empty;
    errdefer current.deinit(allocator);
    var visible: usize = 0;
    var seen_non_space = false;
    var previous_was_space = false;
    var last_break: ?struct {
        line_end: usize,
        carry_start: usize,
        carry_visible_start: usize,
    } = null;
    var i: usize = 0;
    while (i < text.len) {
        if (text[i] == '\x1b') {
            const start = i;
            i += 1;
            while (i < text.len and text[i] != 'm') : (i += 1) {}
            if (i < text.len) i += 1;
            try current.appendSlice(allocator, text[start..i]);
            continue;
        }

        const seq_len = tui_text.utf8SeqLen(text[i]);
        if (i + seq_len > text.len) break;
        const char_width = @max(tui_text.displayWidth(text[i .. i + seq_len]), 1);
        const current_width = if (lines.items.len == 0) first_width else continuation_width;
        if (visible > 0 and visible + char_width > current_width) {
            if (lines.items.len + 1 == max_lines) {
                try current.appendSlice(allocator, text[i..]);
                i = text.len;
                break;
            }
            if (last_break) |breakpoint| {
                if (breakpoint.line_end > 0) {
                    const line = try allocator.dupe(u8, current.items[0..breakpoint.line_end]);
                    errdefer allocator.free(line);
                    const carry = try allocator.dupe(u8, current.items[breakpoint.carry_start..]);
                    defer allocator.free(carry);
                    const carry_visible = visible - breakpoint.carry_visible_start;
                    try lines.append(allocator, line);
                    current.clearRetainingCapacity();
                    try current.appendSlice(allocator, carry);
                    visible = carry_visible;
                    seen_non_space = carry_visible > 0;
                    previous_was_space = false;
                    last_break = null;
                    continue;
                }
            }
            try lines.append(allocator, try current.toOwnedSlice(allocator));
            current = .empty;
            visible = 0;
            seen_non_space = false;
            previous_was_space = false;
            last_break = null;
            continue;
        }
        try current.appendSlice(allocator, text[i .. i + seq_len]);
        visible += char_width;
        const is_space = seq_len == 1 and text[i] == ' ';
        if (is_space) {
            if (seen_non_space) {
                const line_end = if (previous_was_space) (last_break.?.line_end) else current.items.len - seq_len;
                last_break = .{
                    .line_end = line_end,
                    .carry_start = current.items.len,
                    .carry_visible_start = visible,
                };
            }
            previous_was_space = true;
        } else if (char_width > 0) {
            seen_non_space = true;
            previous_was_space = false;
        }
        if (!is_space and seen_non_space and isCodeBreakAfterChar(text[i .. i + seq_len])) {
            last_break = .{
                .line_end = current.items.len,
                .carry_start = current.items.len,
                .carry_visible_start = visible,
            };
        }
        i += seq_len;
    }
    if (current.items.len > 0 or lines.items.len == 0) try lines.append(allocator, try current.toOwnedSlice(allocator));
    return lines.toOwnedSlice(allocator);
}

fn wrapAnsiTextLineCount(allocator: std.mem.Allocator, text: []const u8, first_width: usize, continuation_width: usize) !usize {
    const rows = try wrapAnsiTextWithWidths(allocator, text, first_width, continuation_width, std.math.maxInt(usize));
    defer {
        for (rows) |row| allocator.free(row);
        allocator.free(rows);
    }
    return rows.len;
}

fn isCodeBreakAfterChar(bytes: []const u8) bool {
    return bytes.len == 1 and switch (bytes[0]) {
        ',', ';', ':', '=', ')', ']', '}', '{' => true,
        else => false,
    };
}

fn renderSplitCodeRow(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    store: store_mod.Store,
    row: tui_view.VisualRow,
    width: usize,
    line_width: usize,
    selected: bool,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    const rows = try renderSplitCodeRows(allocator, state, file, file_index, store, row, width, line_width, selected, 1, ansi, palette);
    defer allocator.free(rows);
    return rows[0];
}

fn renderSplitCodeRows(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    store: store_mod.Store,
    row: tui_view.VisualRow,
    width: usize,
    line_width: usize,
    selected: bool,
    max_lines: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![][]u8 {
    if (max_lines == 0) return allocator.alloc([]u8, 0);
    if (width < 32) {
        if (row.right) |right| return renderStackedCodeRows(allocator, state, file, file_index, store, right, width, line_width, selected, max_lines, ansi, palette);
        if (row.left) |left| return renderStackedCodeRows(allocator, state, file, file_index, store, left, width, line_width, selected, max_lines, ansi, palette);
        return oneRenderedLine(allocator, try renderPanelRow(allocator, "", width, selected, ansi, palette));
    }

    const separator_width: usize = 3;
    const side_width = (width - separator_width) / 2;
    const right_width = width - side_width - separator_width;
    const empty_inline_ranges: []const inline_diff.Range = &.{};
    var pair_ranges: ?inline_diff.PairRanges = null;
    if (row.left) |left| {
        if (row.right) |right| {
            if (left.kind == .delete and right.kind == .add) {
                pair_ranges = try inline_diff.diffRanges(allocator, left.text, right.text);
            }
        }
    }
    defer if (pair_ranges) |*ranges| ranges.deinit(allocator);
    const left_inline_ranges = if (pair_ranges) |ranges| ranges.old else empty_inline_ranges;
    const right_inline_ranges = if (pair_ranges) |ranges| ranges.new else empty_inline_ranges;

    const left_rows = try renderSplitCellRows(allocator, state, file, file_index, store, row.left, side_width, line_width, selected, max_lines, left_inline_ranges, ansi, palette);
    defer {
        for (left_rows) |line| allocator.free(line);
        allocator.free(left_rows);
    }
    const right_rows = try renderSplitCellRows(allocator, state, file, file_index, store, row.right, right_width, line_width, selected, max_lines, right_inline_ranges, ansi, palette);
    defer {
        for (right_rows) |line| allocator.free(line);
        allocator.free(right_rows);
    }

    var rows: std.ArrayList([]u8) = .empty;
    errdefer {
        for (rows.items) |line| allocator.free(line);
        rows.deinit(allocator);
    }
    const row_count = @min(max_lines, @max(left_rows.len, right_rows.len));
    for (0..row_count) |i| {
        const left = if (i < left_rows.len) left_rows[i] else try styleCell(allocator, "", side_width, ansi, rowBg(selected, .context, palette), rowFg(.context, palette));
        defer if (i >= left_rows.len) allocator.free(left);
        const right = if (i < right_rows.len) right_rows[i] else try styleCell(allocator, "", right_width, ansi, rowBg(selected, .context, palette), rowFg(.context, palette));
        defer if (i >= right_rows.len) allocator.free(right);
        try rows.append(allocator, try std.fmt.allocPrint(allocator, "{s}{s} │ {s}{s}", .{ left, ansi.reset(), right, ansi.reset() }));
    }
    return rows.toOwnedSlice(allocator);
}

fn renderSplitCell(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    store: store_mod.Store,
    maybe_line: ?*const diff.DiffLine,
    width: usize,
    line_width: usize,
    selected: bool,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    const rows = try renderSplitCellRows(allocator, state, file, file_index, store, maybe_line, width, line_width, selected, 1, &.{}, ansi, palette);
    defer allocator.free(rows);
    return rows[0];
}

fn renderSplitCellRows(
    allocator: std.mem.Allocator,
    state: *State,
    file: diff.DiffFile,
    file_index: usize,
    store: store_mod.Store,
    maybe_line: ?*const diff.DiffLine,
    width: usize,
    line_width: usize,
    selected: bool,
    max_lines: usize,
    inline_ranges: []const inline_diff.Range,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![][]u8 {
    if (max_lines == 0) return allocator.alloc([]u8, 0);
    if (maybe_line) |line| {
        const label = try lineLabel(allocator, line, line_width);
        defer allocator.free(label);
        const highlighted = try renderCodeText(allocator, state, file, file_index, line, ansi, palette);
        defer allocator.free(highlighted);
        const inline_highlighted = try applyInlineRanges(allocator, highlighted, line.text, inline_ranges, ansi, rowBg(selected, line.kind, palette), inlineBg(selected, line.kind, palette));
        defer allocator.free(inline_highlighted);
        const comment_mark = if (lineHasComment(store, file, line)) "!" else " ";
        const prefix = try std.fmt.allocPrint(allocator, "{s} {s}  ", .{ comment_mark, label });
        defer allocator.free(prefix);
        const wrap_widths = lineWrapWidths(width, prefix.len, line.text);
        const continuation_prefix = try blankLabel(allocator, wrap_widths.continuation_prefix);
        defer allocator.free(continuation_prefix);
        const wrapped = try wrapAnsiTextWithWidths(allocator, inline_highlighted, wrap_widths.first, wrap_widths.continuation, max_lines);
        defer {
            for (wrapped) |segment| allocator.free(segment);
            allocator.free(wrapped);
        }
        var rows: std.ArrayList([]u8) = .empty;
        errdefer {
            for (rows.items) |row_text| allocator.free(row_text);
            rows.deinit(allocator);
        }
        for (wrapped, 0..) |segment, i| {
            const raw = try std.fmt.allocPrint(allocator, "{s}{s}", .{ if (i == 0) prefix else continuation_prefix, segment });
            defer allocator.free(raw);
            try rows.append(allocator, try styleCell(allocator, raw, width, ansi, rowBg(selected, line.kind, palette), rowFg(line.kind, palette)));
        }
        return rows.toOwnedSlice(allocator);
    }
    return oneRenderedLine(allocator, try styleCell(allocator, " ", width, ansi, if (selected) palette.bg_selected else palette.bg_default, palette.fg_muted));
}

fn styleCell(
    allocator: std.mem.Allocator,
    text: []const u8,
    width: usize,
    ansi: theme.Ansi,
    bg: theme.Color,
    fg: theme.Color,
) ![]u8 {
    const fitted = try tui_text.fitCell(allocator, text, width);
    defer allocator.free(fitted);
    const bg_code = try ansi.bg(allocator, bg);
    defer allocator.free(bg_code);
    const fg_code = try ansi.fg(allocator, fg);
    defer allocator.free(fg_code);
    const styled = try reapplyStyleAfterResets(allocator, fitted, bg_code, fg_code, ansi.reset());
    defer allocator.free(styled);
    return std.fmt.allocPrint(allocator, "{s}{s}{s}{s}", .{ bg_code, fg_code, styled, ansi.reset() });
}

fn reapplyStyleAfterResets(
    allocator: std.mem.Allocator,
    text: []const u8,
    bg_code: []const u8,
    fg_code: []const u8,
    reset: []const u8,
) ![]u8 {
    if (reset.len == 0) return util.dupe(allocator, text);
    var out: std.ArrayList(u8) = .empty;
    var i: usize = 0;
    while (i < text.len) {
        if (std.mem.startsWith(u8, text[i..], reset)) {
            try out.appendSlice(allocator, reset);
            try out.appendSlice(allocator, bg_code);
            try out.appendSlice(allocator, fg_code);
            i += reset.len;
        } else {
            try out.append(allocator, text[i]);
            i += 1;
        }
    }
    return out.toOwnedSlice(allocator);
}

fn rowBg(selected: bool, kind: diff.DiffLineKind, palette: theme.ThemeTokens) theme.Color {
    if (selected) {
        return switch (kind) {
            .add => .{ .hex = "#285343" },
            .delete => .{ .hex = "#562932" },
            else => palette.bg_selected,
        };
    }
    return switch (kind) {
        .add => palette.diff_add_bg,
        .delete => palette.diff_del_bg,
        else => palette.bg_default,
    };
}

fn inlineBg(selected: bool, kind: diff.DiffLineKind, palette: theme.ThemeTokens) theme.Color {
    _ = palette;
    if (selected) {
        return switch (kind) {
            .add => .{ .hex = "#3c7a5a" },
            .delete => .{ .hex = "#874151" },
            else => .{ .hex = "#45475a" },
        };
    }
    return switch (kind) {
        .add => .{ .hex = "#2f6f4f" },
        .delete => .{ .hex = "#74303d" },
        else => .{ .hex = "#45475a" },
    };
}

fn applyInlineRanges(
    allocator: std.mem.Allocator,
    highlighted: []const u8,
    source_text: []const u8,
    ranges: []const inline_diff.Range,
    ansi: theme.Ansi,
    base_bg: theme.Color,
    inline_bg: theme.Color,
) ![]u8 {
    if (ranges.len == 0 or !ansi.enabled or !ansi.true_color) return util.dupe(allocator, highlighted);

    const base_bg_code = try ansi.bg(allocator, base_bg);
    defer allocator.free(base_bg_code);
    const inline_bg_code = try ansi.bg(allocator, inline_bg);
    defer allocator.free(inline_bg_code);
    if (inline_bg_code.len == 0) return util.dupe(allocator, highlighted);

    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);

    const reset = ansi.reset();
    var source_offset: usize = 0;
    var range_index: usize = 0;
    var active = false;
    var i: usize = 0;
    while (i < highlighted.len) {
        if (highlighted[i] == '\x1b') {
            const start = i;
            i += 1;
            while (i < highlighted.len and highlighted[i] != 'm') : (i += 1) {}
            if (i < highlighted.len) i += 1;
            const sequence = highlighted[start..i];
            try out.appendSlice(allocator, sequence);
            if (active and std.mem.eql(u8, sequence, reset)) try out.appendSlice(allocator, inline_bg_code);
            continue;
        }

        while (range_index < ranges.len and source_offset >= ranges[range_index].end) {
            if (active) {
                try out.appendSlice(allocator, base_bg_code);
                active = false;
            }
            range_index += 1;
        }
        if (!active and range_index < ranges.len and source_offset >= ranges[range_index].start and source_offset < ranges[range_index].end) {
            try out.appendSlice(allocator, inline_bg_code);
            active = true;
        }

        const seq_len = tui_text.utf8SeqLen(highlighted[i]);
        const end = if (i + seq_len <= highlighted.len) i + seq_len else i + 1;
        try out.appendSlice(allocator, highlighted[i..end]);
        if (source_offset < source_text.len) source_offset += end - i;
        i = end;

        while (range_index < ranges.len and source_offset >= ranges[range_index].end) {
            if (active) {
                try out.appendSlice(allocator, base_bg_code);
                active = false;
            }
            range_index += 1;
        }
    }
    if (active) try out.appendSlice(allocator, base_bg_code);
    return out.toOwnedSlice(allocator);
}

fn lineHasComment(store: store_mod.Store, file: diff.DiffFile, line: *const diff.DiffLine) bool {
    const line_number = switch (line.kind) {
        .delete => line.old_lineno,
        .add => line.new_lineno,
        .context => line.new_lineno orelse line.old_lineno,
        .meta => null,
    } orelse return false;
    const side = if (line.kind == .delete) "old" else "new";
    for (store.comments) |comment| {
        if (!util.eql(comment.file_path, file.path)) continue;
        if (comment.start_line != line_number) continue;
        if (util.eql(comment.side, side) or line.kind == .context) return true;
    }
    return false;
}

fn rowFg(kind: diff.DiffLineKind, palette: theme.ThemeTokens) theme.Color {
    return switch (kind) {
        .add => palette.diff_add_fg,
        .delete => palette.diff_del_fg,
        .context => palette.diff_context_fg,
        .meta => palette.fg_muted,
    };
}

fn lineLabel(allocator: std.mem.Allocator, line: *const diff.DiffLine, width: usize) ![]u8 {
    const number = switch (line.kind) {
        .delete => line.old_lineno,
        .add => line.new_lineno,
        .context => line.new_lineno orelse line.old_lineno,
        .meta => null,
    };
    if (number) |n| {
        const raw = try std.fmt.allocPrint(allocator, "{d}", .{n});
        defer allocator.free(raw);
        var out: std.ArrayList(u8) = .empty;
        const padding = if (raw.len < width) width - raw.len else 0;
        for (0..padding) |_| try out.append(allocator, ' ');
        try out.appendSlice(allocator, raw);
        return out.toOwnedSlice(allocator);
    }
    return blankLabel(allocator, width);
}

fn blankLabel(allocator: std.mem.Allocator, width: usize) ![]u8 {
    const out = try allocator.alloc(u8, width);
    @memset(out, ' ');
    return out;
}

fn lineNumberWidth(file: diff.DiffFile) usize {
    var max_line: u32 = 0;
    for (file.hunks) |hunk| {
        for (hunk.lines) |line| {
            if (line.old_lineno) |n| max_line = @max(max_line, n);
            if (line.new_lineno) |n| max_line = @max(max_line, n);
        }
    }
    var width: usize = 1;
    var n = max_line;
    while (n >= 10) : (n /= 10) width += 1;
    return @max(width, 4);
}

fn isSelected(state: *const State, row_index: usize) bool {
    if (row_index == state.cursor_row) return true;
    if (state.selection_start) |start| {
        return row_index >= @min(start, state.cursor_row) and row_index <= @max(start, state.cursor_row);
    }
    return false;
}

const SelectedText = struct {
    bytes: []u8,
    line_count: usize,
};

fn copySelectionToClipboard(
    allocator: std.mem.Allocator,
    io: std.Io,
    snapshot: diff.DiffSnapshot,
    state: *const State,
) !usize {
    const selected = try selectedText(allocator, snapshot, state);
    defer allocator.free(selected.bytes);
    if (selected.bytes.len == 0) return 0;
    try writeOsc52Clipboard(allocator, io, selected.bytes);
    return selected.line_count;
}

fn selectedText(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *const State) !SelectedText {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    if (view.rows.len == 0) return .{ .bytes = try allocator.alloc(u8, 0), .line_count = 0 };

    const cursor = @min(state.cursor_row, view.rows.len - 1);
    const selection_start = if (state.selection_start) |start| @min(start, view.rows.len - 1) else cursor;
    const start = @min(selection_start, cursor);
    const end = @max(selection_start, cursor);
    const file = snapshot.files[state.active_file];

    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);
    var line_count: usize = 0;
    for (start..end + 1) |row_index| {
        try appendCopyRow(allocator, &out, &line_count, file, view, view.rows[row_index]);
    }
    return .{ .bytes = try out.toOwnedSlice(allocator), .line_count = line_count };
}

fn appendCopyRow(
    allocator: std.mem.Allocator,
    out: *std.ArrayList(u8),
    line_count: *usize,
    file: diff.DiffFile,
    view: tui_view.FileView,
    row: tui_view.VisualRow,
) !void {
    switch (row.kind) {
        .file_header => {
            const header = try copyFileHeader(allocator, file, view);
            defer allocator.free(header);
            try appendCopyLine(allocator, out, line_count, header);
        },
        .hunk_header => try appendCopyLine(allocator, out, line_count, row.hunk_header orelse ""),
        .fold => {
            const text = try std.fmt.allocPrint(allocator, "... {d} unmodified lines", .{row.fold_line_count});
            defer allocator.free(text);
            try appendCopyLine(allocator, out, line_count, text);
        },
        .stacked_code, .file_meta => if (row.line) |line| {
            try appendDiffLineCopy(allocator, out, line_count, line);
        },
        .split_code => try appendSplitCopy(allocator, out, line_count, row),
    }
}

fn copyFileHeader(allocator: std.mem.Allocator, file: diff.DiffFile, view: tui_view.FileView) ![]u8 {
    const stats = try fileStats(allocator, view.deletions, view.additions);
    defer allocator.free(stats);
    const path = if (file.old_path) |old|
        if (util.eql(old, file.path)) try util.dupe(allocator, file.path) else try std.fmt.allocPrint(allocator, "{s} -> {s}", .{ old, file.path })
    else
        try util.dupe(allocator, file.path);
    defer allocator.free(path);
    if (stats.len == 0) return util.dupe(allocator, path);
    return std.fmt.allocPrint(allocator, "{s} ({s})", .{ path, stats });
}

fn appendSplitCopy(
    allocator: std.mem.Allocator,
    out: *std.ArrayList(u8),
    line_count: *usize,
    row: tui_view.VisualRow,
) !void {
    if (row.left) |left| {
        if (row.right) |right| {
            if (left == right) {
                try appendDiffLineCopy(allocator, out, line_count, left);
            } else {
                try appendDiffLineCopy(allocator, out, line_count, left);
                try appendDiffLineCopy(allocator, out, line_count, right);
            }
            return;
        }
        try appendDiffLineCopy(allocator, out, line_count, left);
        return;
    }
    if (row.right) |right| try appendDiffLineCopy(allocator, out, line_count, right);
}

fn appendDiffLineCopy(
    allocator: std.mem.Allocator,
    out: *std.ArrayList(u8),
    line_count: *usize,
    line: *const diff.DiffLine,
) !void {
    if (line.kind == .meta) {
        try appendCopyLine(allocator, out, line_count, line.text);
        return;
    }
    const marker: u8 = switch (line.kind) {
        .add => '+',
        .delete => '-',
        .context => ' ',
        .meta => unreachable,
    };
    if (line_count.* > 0) try out.append(allocator, '\n');
    try out.append(allocator, marker);
    try out.appendSlice(allocator, line.text);
    line_count.* += 1;
}

fn appendCopyLine(
    allocator: std.mem.Allocator,
    out: *std.ArrayList(u8),
    line_count: *usize,
    text: []const u8,
) !void {
    if (line_count.* > 0) try out.append(allocator, '\n');
    try out.appendSlice(allocator, text);
    line_count.* += 1;
}

fn writeOsc52Clipboard(allocator: std.mem.Allocator, io: std.Io, text: []const u8) !void {
    const encoded_len = std.base64.standard.Encoder.calcSize(text.len);
    const encoded = try allocator.alloc(u8, encoded_len);
    defer allocator.free(encoded);
    _ = std.base64.standard.Encoder.encode(encoded, text);
    try writeAll(io, "\x1b]52;c;");
    try writeAll(io, encoded);
    try writeAll(io, "\x07");
}

fn renderFileTree(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    store: store_mod.Store,
    state: *const State,
    width: usize,
    height: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![][]u8 {
    if (width == 0) return allocator.alloc([]u8, 0);
    var lines: std.ArrayList([]u8) = .empty;
    errdefer {
        for (lines.items) |line| allocator.free(line);
        lines.deinit(allocator);
    }
    try lines.append(allocator, try styleCell(allocator, " files", width, ansi, palette.bg_panel, palette.fg_muted));
    const start = fileTreeStart(snapshot, state, height);
    const end = @min(snapshot.files.len, start + height - 1);
    for (snapshot.files[start..end], start..) |file, i| {
        const reviewed = store.isReviewed(file.path, file.patch_fingerprint, snapshot.review_target.target_id);
        const comments = store.commentCount(file.path);
        const active = if (i == state.active_file) ">" else " ";
        const mark = if (reviewed) "x" else ".";
        const path_width = if (width > 12) width - 12 else width;
        const path = try tui_text.fitCell(allocator, file.path, path_width);
        defer allocator.free(path);
        const raw = try std.fmt.allocPrint(allocator, "{s}{s} {s} {s} ({d})", .{ active, file.status.label(), mark, path, comments });
        defer allocator.free(raw);
        const line = try styleCell(allocator, raw, width, ansi, if (i == state.active_file) palette.bg_selected else palette.bg_default, if (reviewed) palette.reviewed_badge else palette.unreviewed_badge);
        try lines.append(allocator, line);
    }
    return lines.toOwnedSlice(allocator);
}

fn moveLine(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, delta: isize) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    const total = view.rows.len;
    if (total == 0) return;
    state.cursor_row = moveIndex(state.cursor_row, total, delta);
    try ensureCursorVisible(allocator, snapshot, view, state);
}

fn scrollLines(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, delta: isize) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    const total = view.rows.len;
    if (total == 0) return;
    state.cursor_row = moveIndex(state.cursor_row, total, delta);
    state.scroll_row = moveIndex(state.scroll_row, total, delta);
    try ensureCursorVisible(allocator, snapshot, view, state);
}

const GotoEdge = enum {
    top,
    bottom,
};

const GotoAction = enum {
    none,
    pending,
    top,
    bottom,
};

fn gotoAction(key: []const u8, pending_g: bool) GotoAction {
    if (key.len == 0) return .none;
    if (key[0] == 'G') return .bottom;
    if (key[0] != 'g') return .none;
    if (pending_g or (key.len >= 2 and key[1] == 'g')) return .top;
    return .pending;
}

fn isPlainEscape(key: []const u8) bool {
    return std.mem.eql(u8, key, "\x1b");
}

fn cancelSelectionMode(state: *State) void {
    state.help = false;
    state.pending_g = false;
    state.selection_start = null;
    state.copy_notice = .none;
}

fn jumpToEdge(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, edge: GotoEdge) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    if (view.rows.len == 0) {
        state.cursor_row = 0;
        state.scroll_row = 0;
        return;
    }
    state.cursor_row = switch (edge) {
        .top => 0,
        .bottom => view.rows.len - 1,
    };
    try ensureCursorVisible(allocator, snapshot, view, state);
}

fn moveFile(snapshot: diff.DiffSnapshot, state: *State, delta: isize) void {
    if (snapshot.files.len == 0) return;
    state.active_file = moveIndex(state.active_file, snapshot.files.len, delta);
    state.cursor_row = 0;
    state.scroll_row = 0;
    state.selection_start = null;
}

fn moveIndex(current: usize, len: usize, delta: isize) usize {
    if (len == 0) return 0;
    if (delta < 0) {
        const amount: usize = @intCast(-delta);
        return if (amount > current) 0 else current - amount;
    }
    const next = current + @as(usize, @intCast(delta));
    return @min(next, len - 1);
}

fn ensureCursorVisible(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, view: tui_view.FileView, state: *State) !void {
    const layout = Layout.init(terminalSize());
    try ensureCursorVisibleInLayout(allocator, snapshot, view, state, layout, true);
}

fn ensureRenderViewport(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    view: tui_view.FileView,
    state: *State,
    layout: Layout,
) !void {
    const fill_bottom = !state.preserve_scroll_once;
    try ensureCursorVisibleInLayout(allocator, snapshot, view, state, layout, fill_bottom);
    state.preserve_scroll_once = false;
}

fn ensureCursorVisibleInLayout(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    view: tui_view.FileView,
    state: *State,
    layout: Layout,
    fill_bottom: bool,
) !void {
    const total = view.rows.len;
    if (total == 0) {
        state.scroll_row = 0;
        return;
    }
    if (state.cursor_row < state.scroll_row) state.scroll_row = state.cursor_row;
    if (state.scroll_row >= total) state.scroll_row = total - 1;

    const file = snapshot.files[state.active_file];
    while (state.scroll_row < state.cursor_row) {
        const height = try visualHeightBetween(allocator, snapshot, state, view, state.scroll_row, state.cursor_row + 1, layout);
        if (height <= layout.body_height) break;
        state.scroll_row += 1;
    }

    const scroll_row_height = visualRowHeight(allocator, snapshot, state, file, view.rows[state.scroll_row], layout) catch 1;
    if (scroll_row_height > layout.body_height and state.scroll_row < state.cursor_row) {
        state.scroll_row = state.cursor_row;
    }
    if (fill_bottom) try fillViewportAtBottom(allocator, snapshot, state, view, layout);
}

fn centerCursorInViewport(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, view: tui_view.FileView, state: *State) !void {
    const layout = Layout.init(terminalSize());
    try centerCursorInLayout(allocator, snapshot, view, state, layout);
}

fn centerCursorInLayout(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    view: tui_view.FileView,
    state: *State,
    layout: Layout,
) !void {
    if (view.rows.len == 0) {
        state.scroll_row = 0;
        return;
    }
    state.cursor_row = @min(state.cursor_row, view.rows.len - 1);
    state.scroll_row = try centeredScrollRow(allocator, snapshot, state, view, layout);
    try ensureCursorVisibleInLayout(allocator, snapshot, view, state, layout, false);
}

fn centeredScrollRow(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    layout: Layout,
) !usize {
    const file = snapshot.files[state.active_file];
    const target_offset = layout.body_height / 2;
    var scroll_row = state.cursor_row;
    var height_above: usize = 0;
    while (scroll_row > 0) {
        const prev = scroll_row - 1;
        const prev_height = try visualRowHeight(allocator, snapshot, state, file, view.rows[prev], layout);
        if (height_above + prev_height > target_offset) break;
        height_above += prev_height;
        scroll_row = prev;
    }
    return scroll_row;
}

const SplitCodeAnchor = struct {
    left: ?[]const u8,
    right: ?[]const u8,
};

const CodeRowAnchor = union(enum) {
    stacked: []const u8,
    split: SplitCodeAnchor,
};

const CodeCursorAnchor = struct {
    row: CodeRowAnchor,
    visual_offset: usize,
};

fn captureFoldToggleAnchor(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    target: ?usize,
    layout: Layout,
) !?CodeCursorAnchor {
    if (try captureCodeRowAnchor(allocator, snapshot, state, view, state.cursor_row, layout)) |anchor| return anchor;
    if (target) |target_row| {
        if (target_row < view.rows.len and view.rows[target_row].kind == .fold) {
            if (try captureFirstVisibleCodeAfterFold(allocator, snapshot, state, view, target_row, layout)) |anchor| return anchor;
        }
    }
    if (try captureFirstVisibleCodeAtOrAfter(allocator, snapshot, state, view, state.cursor_row, null, layout)) |anchor| return anchor;
    return captureLastVisibleCodeBefore(allocator, snapshot, state, view, state.cursor_row, layout);
}

fn captureCodeRowAnchor(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    row_index: usize,
    layout: Layout,
) !?CodeCursorAnchor {
    if (row_index >= view.rows.len) return null;
    const row = codeRowAnchor(view.rows[row_index]) orelse return null;
    return .{
        .row = row,
        .visual_offset = (try visualOffsetIfVisible(allocator, snapshot, state, view, row_index, layout)) orelse 0,
    };
}

fn restoreCodeCursorAnchor(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *State,
    view: tui_view.FileView,
    anchor: ?CodeCursorAnchor,
    layout: Layout,
) !bool {
    const captured = anchor orelse return false;
    const row_index = findCodeRowByAnchor(view, captured.row) orelse return false;
    state.cursor_row = row_index;
    state.scroll_row = try scrollRowForVisualOffset(allocator, snapshot, state, view, row_index, captured.visual_offset, layout);
    state.preserve_scroll_once = true;
    try ensureCursorVisibleInLayout(allocator, snapshot, view, state, layout, false);
    return true;
}

fn scrollRowForVisualOffset(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    row_index: usize,
    visual_offset: usize,
    layout: Layout,
) !usize {
    if (view.rows.len == 0) return 0;
    const file = snapshot.files[state.active_file];
    var scroll_row = @min(row_index, view.rows.len - 1);
    var height_above: usize = 0;
    while (scroll_row > 0) {
        const prev = scroll_row - 1;
        const prev_height = try visualRowHeight(allocator, snapshot, state, file, view.rows[prev], layout);
        if (height_above + prev_height > visual_offset) break;
        height_above += prev_height;
        scroll_row = prev;
    }
    return scroll_row;
}

fn captureFirstVisibleCodeAfterFold(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    fold_row: usize,
    layout: Layout,
) !?CodeCursorAnchor {
    if (fold_row >= view.rows.len) return null;
    const fold_id = view.rows[fold_row].fold_id;
    return captureFirstVisibleCodeAtOrAfter(allocator, snapshot, state, view, fold_row + 1, fold_id, layout);
}

fn captureFirstVisibleCodeAtOrAfter(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    start_row: usize,
    skip_fold_id: ?tui_view.FoldId,
    layout: Layout,
) !?CodeCursorAnchor {
    if (view.rows.len == 0) return null;
    var row_index = @max(start_row, state.scroll_row);
    while (row_index < view.rows.len) : (row_index += 1) {
        const visual_offset = (try visualOffsetIfVisible(allocator, snapshot, state, view, row_index, layout)) orelse break;
        if (skip_fold_id) |fold_id| {
            if (rowBelongsToFold(view.rows[row_index], fold_id)) continue;
        }
        const row = codeRowAnchor(view.rows[row_index]) orelse continue;
        return .{ .row = row, .visual_offset = visual_offset };
    }
    return null;
}

fn captureLastVisibleCodeBefore(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    start_row: usize,
    layout: Layout,
) !?CodeCursorAnchor {
    if (view.rows.len == 0 or state.scroll_row >= view.rows.len) return null;
    var row_index = @min(start_row, view.rows.len - 1);
    while (row_index >= state.scroll_row) : (row_index -= 1) {
        const visual_offset = (try visualOffsetIfVisible(allocator, snapshot, state, view, row_index, layout)) orelse return null;
        if (codeRowAnchor(view.rows[row_index])) |row| return .{ .row = row, .visual_offset = visual_offset };
        if (row_index == 0) break;
    }
    return null;
}

fn visualOffsetIfVisible(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    row_index: usize,
    layout: Layout,
) !?usize {
    if (view.rows.len == 0 or state.scroll_row >= view.rows.len or row_index < state.scroll_row or row_index >= view.rows.len) return null;
    const file = snapshot.files[state.active_file];
    var visual_offset: usize = 0;
    var i = state.scroll_row;
    while (i < row_index) : (i += 1) {
        visual_offset += try visualRowHeight(allocator, snapshot, state, file, view.rows[i], layout);
        if (visual_offset >= layout.body_height) return null;
    }
    return visual_offset;
}

fn rowBelongsToFold(row: tui_view.VisualRow, fold_id: tui_view.FoldId) bool {
    const row_fold_id = row.fold_id orelse return false;
    return row_fold_id.eql(fold_id);
}

fn codeRowAnchor(row: tui_view.VisualRow) ?CodeRowAnchor {
    return switch (row.kind) {
        .stacked_code => if (row.line) |line| .{ .stacked = line.stable_line_id } else null,
        .split_code => splitCodeAnchor(row),
        else => null,
    };
}

fn splitCodeAnchor(row: tui_view.VisualRow) ?CodeRowAnchor {
    const left = if (row.left) |line| line.stable_line_id else null;
    const right = if (row.right) |line| line.stable_line_id else null;
    if (left == null and right == null) return null;
    return .{ .split = .{ .left = left, .right = right } };
}

fn findCodeRowByAnchor(view: tui_view.FileView, anchor: CodeRowAnchor) ?usize {
    for (view.rows, 0..) |row, i| {
        const candidate = codeRowAnchor(row) orelse continue;
        if (codeRowAnchorEql(candidate, anchor)) return i;
    }
    return null;
}

fn codeRowAnchorEql(a: CodeRowAnchor, b: CodeRowAnchor) bool {
    switch (a) {
        .stacked => |a_line| switch (b) {
            .stacked => |b_line| return util.eql(a_line, b_line),
            .split => return false,
        },
        .split => |a_split| switch (b) {
            .stacked => return false,
            .split => |b_split| return optionalSliceEql(a_split.left, b_split.left) and optionalSliceEql(a_split.right, b_split.right),
        },
    }
}

fn optionalSliceEql(a: ?[]const u8, b: ?[]const u8) bool {
    if (a) |a_value| {
        if (b) |b_value| return util.eql(a_value, b_value);
        return false;
    }
    return b == null;
}

fn visualHeightBetween(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    start: usize,
    end: usize,
    layout: Layout,
) !usize {
    const file = snapshot.files[state.active_file];
    var height: usize = 0;
    for (view.rows[start..end]) |row| height += try visualRowHeight(allocator, snapshot, state, file, row, layout);
    return height;
}

fn fillViewportAtBottom(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *State,
    view: tui_view.FileView,
    layout: Layout,
) !void {
    if (view.rows.len == 0 or state.scroll_row >= view.rows.len) return;
    var height = try visualHeightBetween(allocator, snapshot, state, view, state.scroll_row, view.rows.len, layout);
    while (state.scroll_row > 0 and height < layout.body_height) {
        const prev = state.scroll_row - 1;
        const file = snapshot.files[state.active_file];
        const prev_height = try visualRowHeight(allocator, snapshot, state, file, view.rows[prev], layout);
        if (height + prev_height > layout.body_height) break;
        state.scroll_row = prev;
        height += prev_height;
    }
}

fn rowAtVisualOffset(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    view: tui_view.FileView,
    offset: usize,
    layout: Layout,
) !usize {
    const file = snapshot.files[state.active_file];
    var visual_y: usize = 0;
    var row_index = @min(state.scroll_row, view.rows.len - 1);
    while (row_index < view.rows.len) : (row_index += 1) {
        const row_height = try visualRowHeight(allocator, snapshot, state, file, view.rows[row_index], layout);
        if (offset < visual_y + row_height) return row_index;
        visual_y += row_height;
    }
    return view.rows.len - 1;
}

fn visualRowHeight(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    state: *const State,
    file: diff.DiffFile,
    row: tui_view.VisualRow,
    layout: Layout,
) !usize {
    _ = snapshot;
    const line_width = lineNumberWidth(file);
    return switch (row.kind) {
        .file_header, .hunk_header, .fold => 1,
        .stacked_code, .file_meta => if (row.line) |line|
            try stackedLineHeight(allocator, line, layout.main_width, line_width)
        else
            1,
        .split_code => try splitLineHeight(allocator, state, row, layout.main_width, line_width),
    };
}

fn stackedLineHeight(allocator: std.mem.Allocator, line: *const diff.DiffLine, width: usize, line_width: usize) !usize {
    const prefix_width = 2 + 1 + line_width + 2;
    const wrap_widths = lineWrapWidths(width, prefix_width, line.text);
    return wrapAnsiTextLineCount(allocator, line.text, wrap_widths.first, wrap_widths.continuation);
}

fn splitLineHeight(allocator: std.mem.Allocator, state: *const State, row: tui_view.VisualRow, width: usize, line_width: usize) !usize {
    _ = state;
    if (width < 32) {
        if (row.right) |right| return stackedLineHeight(allocator, right, width, line_width);
        if (row.left) |left| return stackedLineHeight(allocator, left, width, line_width);
        return 1;
    }
    const separator_width: usize = 3;
    const side_width = (width - separator_width) / 2;
    const right_width = width - side_width - separator_width;
    const left_height = if (row.left) |left| try splitCellHeight(allocator, left, side_width, line_width) else 1;
    const right_height = if (row.right) |right| try splitCellHeight(allocator, right, right_width, line_width) else 1;
    return @max(left_height, right_height);
}

fn splitCellHeight(allocator: std.mem.Allocator, line: *const diff.DiffLine, width: usize, line_width: usize) !usize {
    const prefix_width = 1 + 1 + line_width + 2;
    const wrap_widths = lineWrapWidths(width, prefix_width, line.text);
    return wrapAnsiTextLineCount(allocator, line.text, wrap_widths.first, wrap_widths.continuation);
}

fn jumpUnreviewed(snapshot: diff.DiffSnapshot, store: store_mod.Store, state: *State) void {
    for (snapshot.files, 0..) |file, i| {
        if (!store.isReviewed(file.path, file.patch_fingerprint, snapshot.review_target.target_id)) {
            state.active_file = i;
            state.cursor_row = 0;
            state.scroll_row = 0;
            return;
        }
    }
}

fn fileTreeStart(snapshot: diff.DiffSnapshot, state: *const State, height: usize) usize {
    return fileTreeStartIndex(snapshot.files.len, state.active_file, height);
}

fn fileTreeStartIndex(file_count: usize, active_file: usize, height: usize) usize {
    if (height <= 2 or active_file < height - 1) return 0;
    return @min(active_file - (height - 2), file_count);
}

fn currentView(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *const State) !tui_view.FileView {
    return tui_view.buildFileView(allocator, &snapshot.files[state.active_file], state.active_file, state.mode, state.folds.items);
}

fn clampCursor(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    if (view.rows.len == 0) {
        state.cursor_row = 0;
        state.scroll_row = 0;
        return;
    }
    state.cursor_row = @min(state.cursor_row, view.rows.len - 1);
    state.scroll_row = @min(state.scroll_row, view.rows.len - 1);
    try ensureCursorVisible(allocator, snapshot, view, state);
}

fn jumpChange(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, delta: isize) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    const next = if (delta > 0) tui_view.nextChange(view, state.cursor_row) else tui_view.previousChange(view, state.cursor_row);
    if (next) |row| {
        state.cursor_row = row;
        if (delta > 0) {
            try centerCursorInViewport(allocator, snapshot, view, state);
        } else {
            try ensureCursorVisible(allocator, snapshot, view, state);
        }
    }
}

fn toggleCurrentFold(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State) !void {
    try toggleCurrentFoldInLayout(allocator, snapshot, state, Layout.init(terminalSize()));
}

fn toggleCurrentFoldInLayout(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, layout: Layout) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    if (state.cursor_row >= view.rows.len) return;
    const target = findFoldTarget(view, state.cursor_row) orelse return;
    const cursor_anchor = try captureFoldToggleAnchor(allocator, snapshot, state, view, target, layout);
    const id = view.rows[target].fold_id orelse return;
    try toggleFold(state, allocator, id);
    var updated = try currentView(allocator, snapshot, state);
    defer updated.deinit(allocator);
    if (try restoreCodeCursorAnchor(allocator, snapshot, state, updated, cursor_anchor, layout)) return;
    if (findFoldRowById(updated, id)) |row| state.cursor_row = row else state.cursor_row = @min(target, if (updated.rows.len == 0) 0 else updated.rows.len - 1);
    try ensureCursorVisibleInLayout(allocator, snapshot, updated, state, layout, true);
}

fn findFoldTarget(view: tui_view.FileView, cursor_row: usize) ?usize {
    if (view.rows.len == 0) return null;
    const start = @min(cursor_row, view.rows.len - 1);
    return if (view.rows[start].kind == .fold) start else null;
}

fn findFoldRowById(view: tui_view.FileView, id: tui_view.FoldId) ?usize {
    for (view.rows, 0..) |row, i| {
        if (row.kind == .fold) {
            if (row.fold_id) |row_id| {
                if (row_id.eql(id)) return i;
            }
        }
    }
    return null;
}

fn toggleAllFolds(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State) !void {
    try toggleAllFoldsInLayout(allocator, snapshot, state, Layout.init(terminalSize()));
}

fn toggleAllFoldsInLayout(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, layout: Layout) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    const target = findFoldTarget(view, state.cursor_row);
    const cursor_anchor = try captureFoldToggleAnchor(allocator, snapshot, state, view, target, layout);
    var should_expand = false;
    for (view.rows) |row| {
        if (row.kind == .fold and !row.fold_expanded) {
            should_expand = true;
            break;
        }
    }
    for (view.rows) |row| {
        if (row.kind == .fold) {
            try setFold(state, allocator, row.fold_id.?, if (should_expand) .expanded else .collapsed);
        }
    }
    var updated = try currentView(allocator, snapshot, state);
    defer updated.deinit(allocator);
    if (try restoreCodeCursorAnchor(allocator, snapshot, state, updated, cursor_anchor, layout)) return;
    try clampCursor(allocator, snapshot, state);
}

fn toggleFold(state: *State, allocator: std.mem.Allocator, id: tui_view.FoldId) !void {
    for (state.folds.items) |*entry| {
        if (entry.id.eql(id)) {
            entry.state = if (entry.state == .expanded) .collapsed else .expanded;
            return;
        }
    }
    try state.folds.append(allocator, .{ .id = id, .state = .expanded });
}

fn setFold(state: *State, allocator: std.mem.Allocator, id: tui_view.FoldId, fold_state: tui_view.FoldState) !void {
    for (state.folds.items) |*entry| {
        if (entry.id.eql(id)) {
            entry.state = fold_state;
            return;
        }
    }
    try state.folds.append(allocator, .{ .id = id, .state = fold_state });
}

fn addCommentAtCursor(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    store: *store_mod.Store,
    state: *const State,
    body: []const u8,
    author: []const u8,
) !bool {
    const file = snapshot.files[state.active_file];
    var view = try tui_view.buildFileView(allocator, &file, state.active_file, state.mode, state.folds.items);
    defer view.deinit(allocator);
    if (state.cursor_row >= view.rows.len) return false;
    const row = view.rows[state.cursor_row];
    const line = row.commentLine() orelse return false;
    const hunk_header = if (row.hunk_index) |idx| file.hunks[idx].header else "";
    var end_line: u32 = 0;
    if (state.selection_start) |start| {
        const last_row_index = @min(@max(start, state.cursor_row), view.rows.len - 1);
        if (view.rows[last_row_index].commentLine()) |last| end_line = if (last.kind == .delete) (last.old_lineno orelse 0) else (last.new_lineno orelse last.old_lineno orelse 0);
    }
    var comment = try store.addComment(snapshot.repository.repo_id, snapshot.review_target.target_id, file, line, hunk_header, end_line, body, author);
    defer comment.deinit(allocator);
    return true;
}

fn readEvent() ?Event {
    var buf: [64]u8 = undefined;
    const n = std.posix.read(std.posix.STDIN_FILENO, &buf) catch return null;
    if (n == 0) return null;
    const bytes = buf[0..n];
    if (parseSgrMouse(bytes)) |mouse| return .{ .mouse = mouse };
    return .{ .key = bytes };
}

fn parseSgrMouse(bytes: []const u8) ?MouseEvent {
    if (!std.mem.startsWith(u8, bytes, "\x1b[<")) return null;
    var i: usize = 3;
    const button_start = i;
    while (i < bytes.len and std.ascii.isDigit(bytes[i])) : (i += 1) {}
    if (i == button_start or i >= bytes.len or bytes[i] != ';') return null;
    const button = std.fmt.parseInt(u16, bytes[button_start..i], 10) catch return null;
    i += 1;
    const x_start = i;
    while (i < bytes.len and std.ascii.isDigit(bytes[i])) : (i += 1) {}
    if (i == x_start or i >= bytes.len or bytes[i] != ';') return null;
    const x = std.fmt.parseInt(usize, bytes[x_start..i], 10) catch return null;
    i += 1;
    const y_start = i;
    while (i < bytes.len and std.ascii.isDigit(bytes[i])) : (i += 1) {}
    if (i == y_start or i >= bytes.len) return null;
    const y = std.fmt.parseInt(usize, bytes[y_start..i], 10) catch return null;
    if (bytes[i] != 'M' and bytes[i] != 'm') return null;
    return .{ .button = button, .x = x, .y = y, .pressed = bytes[i] == 'M' };
}

fn terminalSize() Size {
    if (builtin.os.tag == .linux) {
        var wsz: std.posix.winsize = undefined;
        const rc = std.os.linux.ioctl(std.posix.STDOUT_FILENO, std.os.linux.T.IOCGWINSZ, @intFromPtr(&wsz));
        if (std.os.linux.errno(rc) == .SUCCESS and wsz.col > 0 and wsz.row > 0) {
            return .{ .width = wsz.col, .height = wsz.row };
        }
    }
    if (terminalSizeFromPosixIoctl()) |size| return size;
    return .{ .width = terminalEnv("COLUMNS", 100), .height = terminalEnv("LINES", 32) };
}

fn terminalSizeFromPosixIoctl() ?Size {
    if (builtin.os.tag == .windows) return null;
    var wsz: std.posix.winsize = .{
        .row = 0,
        .col = 0,
        .xpixel = 0,
        .ypixel = 0,
    };
    const rc = std.posix.system.ioctl(
        std.posix.STDOUT_FILENO,
        @intCast(std.posix.T.IOCGWINSZ),
        @intFromPtr(&wsz),
    );
    if (rc != 0 or wsz.col == 0 or wsz.row == 0) return null;
    return .{ .width = wsz.col, .height = wsz.row };
}

fn terminalEnv(comptime name: []const u8, fallback: usize) usize {
    const value = (util.envOwned(std.heap.page_allocator, name) catch null) orelse return fallback;
    defer std.heap.page_allocator.free(value);
    return std.fmt.parseInt(usize, value, 10) catch fallback;
}

const CommentInput = struct {
    body: std.ArrayList(u8) = .empty,
    cursor: usize = 0,

    fn deinit(self: *CommentInput, allocator: std.mem.Allocator) void {
        self.body.deinit(allocator);
    }

    fn appendByte(self: *CommentInput, allocator: std.mem.Allocator, byte: u8) !void {
        try self.appendSlice(allocator, &.{byte});
    }

    fn appendSlice(self: *CommentInput, allocator: std.mem.Allocator, bytes: []const u8) !void {
        try self.body.insertSlice(allocator, self.cursor, bytes);
        self.cursor += bytes.len;
    }

    fn appendNewline(self: *CommentInput, allocator: std.mem.Allocator) !void {
        try self.appendByte(allocator, '\n');
    }

    fn backspace(self: *CommentInput) void {
        if (self.cursor == 0) return;
        const start = previousUtf8Boundary(self.body.items, self.cursor);
        self.removeRange(start, self.cursor);
        self.cursor = start;
    }

    fn deleteForward(self: *CommentInput) void {
        if (self.cursor >= self.body.items.len) return;
        self.removeRange(self.cursor, nextUtf8Boundary(self.body.items, self.cursor));
    }

    fn moveLeft(self: *CommentInput) void {
        self.cursor = previousUtf8Boundary(self.body.items, self.cursor);
    }

    fn moveRight(self: *CommentInput) void {
        self.cursor = nextUtf8Boundary(self.body.items, self.cursor);
    }

    fn moveUp(self: *CommentInput) void {
        self.cursor = verticalCursorMove(self.body.items, self.cursor, .up);
    }

    fn moveDown(self: *CommentInput) void {
        self.cursor = verticalCursorMove(self.body.items, self.cursor, .down);
    }

    fn takeOwned(self: *CommentInput, allocator: std.mem.Allocator) ![]u8 {
        const owned = try self.body.toOwnedSlice(allocator);
        self.body = .empty;
        self.cursor = 0;
        return owned;
    }

    fn removeRange(self: *CommentInput, start: usize, end: usize) void {
        if (start >= end or end > self.body.items.len) return;
        const len = end - start;
        std.mem.copyForwards(u8, self.body.items[start .. self.body.items.len - len], self.body.items[end..]);
        self.body.items.len -= len;
    }
};

const CommentInputAction = enum {
    continue_input,
    incomplete_escape,
    save,
    cancel,
    quit,
};

const CommentInputResult = struct {
    action: CommentInputAction,
    consumed: usize,
};

const comment_save_sequences = [_][]const u8{
    "\x1b[13;5u",
    "\x1b[10;5u",
    "\x1b[13;5~",
    "\x1b[10;5~",
    "\x1b[27;5;13~",
    "\x1b[27;5;10~",
};

const comment_navigation_sequences = [_][]const u8{
    "\x1b[D",
    "\x1b[C",
    "\x1b[A",
    "\x1b[B",
    "\x1b[3~",
};

fn handleCommentInputBytes(allocator: std.mem.Allocator, input: *CommentInput, bytes: []const u8) !CommentInputAction {
    const result = try handleCommentInputBytesPartial(allocator, input, bytes);
    return if (result.action == .incomplete_escape) .continue_input else result.action;
}

fn handleCommentInputBytesPartial(allocator: std.mem.Allocator, input: *CommentInput, bytes: []const u8) !CommentInputResult {
    var i: usize = 0;
    while (i < bytes.len) {
        switch (bytes[i]) {
            3 => return .{ .action = .quit, .consumed = i + 1 },
            4 => {},
            27 => {
                const remaining = bytes[i..];
                if (matchingEscapeSequence(remaining, comment_save_sequences[0..])) |len| return .{ .action = .save, .consumed = i + len };
                if (std.mem.startsWith(u8, bytes[i..], "\x1b[D")) {
                    input.moveLeft();
                    i += 3;
                    continue;
                }
                if (std.mem.startsWith(u8, bytes[i..], "\x1b[C")) {
                    input.moveRight();
                    i += 3;
                    continue;
                }
                if (std.mem.startsWith(u8, bytes[i..], "\x1b[A")) {
                    input.moveUp();
                    i += 3;
                    continue;
                }
                if (std.mem.startsWith(u8, bytes[i..], "\x1b[B")) {
                    input.moveDown();
                    i += 3;
                    continue;
                }
                if (std.mem.startsWith(u8, bytes[i..], "\x1b[3~")) {
                    input.deleteForward();
                    i += 4;
                    continue;
                }
                if (remaining.len == 1) return .{ .action = .cancel, .consumed = i + 1 };
                if (isEscapeSequencePrefix(remaining, comment_save_sequences[0..]) or
                    isEscapeSequencePrefix(remaining, comment_navigation_sequences[0..]))
                {
                    return .{ .action = .incomplete_escape, .consumed = i };
                }
                return .{ .action = .cancel, .consumed = i + 1 };
            },
            '\r' => {
                try input.appendNewline(allocator);
                i += 1;
                if (i < bytes.len and bytes[i] == '\n') i += 1;
                continue;
            },
            '\n' => {
                try input.appendNewline(allocator);
                i += 1;
                continue;
            },
            127, 8 => input.backspace(),
            '\t' => try input.appendByte(allocator, bytes[i]),
            else => {
                if (bytes[i] >= 0x20) {
                    const seq_len = tui_text.utf8SeqLen(bytes[i]);
                    if (i + seq_len <= bytes.len) {
                        try input.appendSlice(allocator, bytes[i .. i + seq_len]);
                        i += seq_len;
                        continue;
                    }
                    try input.appendByte(allocator, bytes[i]);
                }
            },
        }
        i += 1;
    }
    return .{ .action = .continue_input, .consumed = bytes.len };
}

fn matchingEscapeSequence(bytes: []const u8, sequences: []const []const u8) ?usize {
    for (sequences) |sequence| {
        if (std.mem.startsWith(u8, bytes, sequence)) return sequence.len;
    }
    return null;
}

fn isEscapeSequencePrefix(bytes: []const u8, sequences: []const []const u8) bool {
    for (sequences) |sequence| {
        if (bytes.len < sequence.len and std.mem.startsWith(u8, sequence, bytes)) return true;
    }
    return false;
}

fn discardCommentInputPrefix(bytes: *std.ArrayList(u8), count: usize) void {
    if (count == 0) return;
    if (count >= bytes.items.len) {
        bytes.clearRetainingCapacity();
        return;
    }
    std.mem.copyForwards(u8, bytes.items[0 .. bytes.items.len - count], bytes.items[count..]);
    bytes.items.len -= count;
}

fn readCommentBody(
    allocator: std.mem.Allocator,
    io: std.Io,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) !?[]u8 {
    var input = CommentInput{};
    defer input.deinit(allocator);
    var pending: std.ArrayList(u8) = .empty;
    defer pending.deinit(allocator);
    try writeAll(io, enable_extended_keyboard);
    defer writeAll(io, disable_extended_keyboard) catch {};
    try drawCommentEditor(allocator, io, input, ansi, palette);
    while (true) {
        var bytes: [64]u8 = undefined;
        const n = try std.posix.read(std.posix.STDIN_FILENO, &bytes);
        if (n == 0) return try input.takeOwned(allocator);
        try pending.appendSlice(allocator, bytes[0..n]);
        while (pending.items.len > 0) {
            const result = try handleCommentInputBytesPartial(allocator, &input, pending.items);
            discardCommentInputPrefix(&pending, result.consumed);
            switch (result.action) {
                .continue_input => break,
                .incomplete_escape => break,
                .save => return try input.takeOwned(allocator),
                .cancel => return null,
                .quit => return error.Interrupted,
            }
        }
        try drawCommentEditor(allocator, io, input, ansi, palette);
    }
}

fn drawCommentEditor(
    allocator: std.mem.Allocator,
    io: std.Io,
    input: CommentInput,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) !void {
    const layout = CommentEditorLayout.init(terminalSize());
    const body = input.body.items;
    const start = commentPreviewStartForCursor(body, input.cursor, layout.body_rows);

    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    try appendEditorBackdrop(allocator, &out, layout, ansi, palette);
    try appendStyledEditorLine(allocator, &out, layout.y, layout.x, try editorBorder(allocator, layout.width), layout.width, ansi, palette, false);

    var rendered: usize = 0;
    var cursor = start;
    while (rendered < layout.body_rows) : (rendered += 1) {
        if (cursor < body.len) {
            const remaining = body[cursor..];
            const line_end = std.mem.indexOfScalar(u8, remaining, '\n') orelse remaining.len;
            try appendEditorLine(allocator, &out, layout, layout.body_y + rendered, remaining[0..line_end], ansi, palette, false);
            cursor += line_end;
            if (cursor < body.len and body[cursor] == '\n') cursor += 1;
        } else {
            try appendEditorLine(allocator, &out, layout, layout.body_y + rendered, "", ansi, palette, false);
        }
    }

    var spacer = layout.body_y + rendered;
    while (spacer < layout.height - 2) : (spacer += 1) {
        try appendEditorLine(allocator, &out, layout, spacer, "", ansi, palette, false);
    }

    try appendEditorLine(allocator, &out, layout, layout.height - 2, "Enter newline  Ctrl+Enter save  Esc cancel", ansi, palette, true);
    try appendStyledEditorLine(allocator, &out, layout.y + layout.height - 1, layout.x, try editorBorder(allocator, layout.width), layout.width, ansi, palette, false);

    const cursor_pos = commentCursorPosition(body, input.cursor, start, layout);
    try appendCursorMove(allocator, &out, cursor_pos.y, cursor_pos.x);
    try writeAll(io, out.items);
}

const CommentEditorLayout = struct {
    panel_x: usize,
    panel_y: usize,
    panel_width: usize,
    panel_height: usize,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    content_x: usize,
    content_width: usize,
    body_y: usize,
    body_rows: usize,

    fn init(size: Size) CommentEditorLayout {
        const terminal_width = @max(size.width, @as(usize, 60));
        const terminal_height = @max(size.height, @as(usize, 8));
        const width = @min(@as(usize, 84), terminal_width - 4);
        const height = @min(@as(usize, 14), terminal_height - 2);
        const x = ((terminal_width - width) / 2) + 1;
        const y = ((terminal_height - height) / 2) + 1;
        const panel_pad_x = @min(@as(usize, 8), x - 1);
        const panel_pad_top = @min(@as(usize, 1), y - 1);
        const panel_x = x - panel_pad_x;
        const panel_y = y - panel_pad_top;
        const panel_width = @min((terminal_width -| panel_x) +| 1, width +| panel_pad_x +| panel_pad_x);
        const panel_height = @min((terminal_height -| panel_y) +| 1, height +| panel_pad_top +| 1);
        return .{
            .panel_x = panel_x,
            .panel_y = panel_y,
            .panel_width = panel_width,
            .panel_height = panel_height,
            .x = x,
            .y = y,
            .width = width,
            .height = height,
            .content_x = 3,
            .content_width = width - 5,
            .body_y = 2,
            .body_rows = height - 4,
        };
    }
};

const CommentCursorPosition = struct {
    x: usize,
    y: usize,
};

const VerticalDirection = enum {
    up,
    down,
};

fn previousUtf8Boundary(bytes: []const u8, cursor: usize) usize {
    if (cursor == 0) return 0;
    var i = @min(cursor, bytes.len) - 1;
    while (i > 0 and (bytes[i] & 0xc0) == 0x80) : (i -= 1) {}
    return i;
}

fn nextUtf8Boundary(bytes: []const u8, cursor: usize) usize {
    if (cursor >= bytes.len) return bytes.len;
    const seq_len = tui_text.utf8SeqLen(bytes[cursor]);
    return @min(bytes.len, cursor + seq_len);
}

fn lineStartBefore(bytes: []const u8, cursor: usize) usize {
    var i = @min(cursor, bytes.len);
    while (i > 0) {
        if (bytes[i - 1] == '\n') return i;
        i -= 1;
    }
    return 0;
}

fn lineEndAfter(bytes: []const u8, cursor: usize) usize {
    var i = @min(cursor, bytes.len);
    while (i < bytes.len and bytes[i] != '\n') : (i += 1) {}
    return i;
}

fn displayWidthRange(bytes: []const u8, start: usize, end: usize) usize {
    var width: usize = 0;
    var i = start;
    while (i < end) {
        const seq_len = tui_text.utf8SeqLen(bytes[i]);
        if (i + seq_len > end) break;
        width += tui_text.displayWidth(bytes[i .. i + seq_len]);
        i += seq_len;
    }
    return width;
}

fn cursorForColumn(bytes: []const u8, line_start: usize, line_end: usize, target_column: usize) usize {
    var width: usize = 0;
    var i = line_start;
    while (i < line_end) {
        const seq_len = tui_text.utf8SeqLen(bytes[i]);
        if (i + seq_len > line_end) break;
        const next_width = width + tui_text.displayWidth(bytes[i .. i + seq_len]);
        if (next_width > target_column) break;
        width = next_width;
        i += seq_len;
    }
    return i;
}

fn verticalCursorMove(bytes: []const u8, cursor: usize, direction: VerticalDirection) usize {
    const current_line_start = lineStartBefore(bytes, cursor);
    const current_line_end = lineEndAfter(bytes, cursor);
    const target_column = displayWidthRange(bytes, current_line_start, cursor);

    switch (direction) {
        .up => {
            if (current_line_start == 0) return cursor;
            const previous_line_end = current_line_start - 1;
            const previous_line_start = lineStartBefore(bytes, previous_line_end);
            return cursorForColumn(bytes, previous_line_start, previous_line_end, target_column);
        },
        .down => {
            if (current_line_end >= bytes.len) return cursor;
            const next_line_start = current_line_end + 1;
            const next_line_end = lineEndAfter(bytes, next_line_start);
            return cursorForColumn(bytes, next_line_start, next_line_end, target_column);
        },
    }
}

fn commentCursorPosition(body: []const u8, cursor: usize, start: usize, layout: CommentEditorLayout) CommentCursorPosition {
    var row: usize = 0;
    var line_start = start;
    var i = start;
    while (i < cursor and i < body.len) : (i += 1) {
        if (body[i] == '\n') {
            if (row + 1 < layout.body_rows) {
                row += 1;
                line_start = i + 1;
            }
        }
    }

    const visible_width = displayWidthRange(body, line_start, @min(cursor, body.len));

    return .{
        .x = layout.x + layout.content_x + @min(layout.content_width, visible_width),
        .y = layout.y + layout.body_y + @min(row, layout.body_rows - 1),
    };
}

fn appendCursorMove(allocator: std.mem.Allocator, out: *std.ArrayList(u8), y: usize, x: usize) !void {
    const cursor_move = try std.fmt.allocPrint(allocator, "\x1b[{d};{d}H", .{ y, x });
    defer allocator.free(cursor_move);
    try out.appendSlice(allocator, cursor_move);
}

fn appendBoxBorder(allocator: std.mem.Allocator, out: *std.ArrayList(u8), width: usize) !void {
    try out.append(allocator, '+');
    var i: usize = 2;
    while (i < width) : (i += 1) try out.append(allocator, '-');
    try out.append(allocator, '+');
}

fn editorBlankLine(allocator: std.mem.Allocator, width: usize) ![]u8 {
    const line = try allocator.alloc(u8, width);
    @memset(line, ' ');
    return line;
}

fn editorBorder(allocator: std.mem.Allocator, width: usize) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);
    try appendBoxBorder(allocator, &out, width);
    return out.toOwnedSlice(allocator);
}

fn appendEditorBackdrop(
    allocator: std.mem.Allocator,
    out: *std.ArrayList(u8),
    layout: CommentEditorLayout,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) !void {
    var row: usize = 0;
    while (row < layout.panel_height) : (row += 1) {
        try appendStyledEditorLine(allocator, out, layout.panel_y + row, layout.panel_x, try editorBlankLine(allocator, layout.panel_width), layout.panel_width, ansi, palette, false);
    }
}

fn appendStyledEditorLine(
    allocator: std.mem.Allocator,
    out: *std.ArrayList(u8),
    y: usize,
    x: usize,
    text: []u8,
    width: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
    muted: bool,
) !void {
    defer allocator.free(text);
    try appendCursorMove(allocator, out, y, x);
    const styled = try styleCell(allocator, text, width, ansi, palette.bg_panel, if (muted) palette.fg_muted else palette.fg_default);
    defer allocator.free(styled);
    try out.appendSlice(allocator, styled);
}

fn appendEditorLine(
    allocator: std.mem.Allocator,
    out: *std.ArrayList(u8),
    layout: CommentEditorLayout,
    row_offset: usize,
    text: []const u8,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
    muted: bool,
) !void {
    var raw: std.ArrayList(u8) = .empty;
    errdefer raw.deinit(allocator);
    try raw.append(allocator, '|');
    var pad: usize = 1;
    while (pad < layout.content_x) : (pad += 1) try raw.append(allocator, ' ');
    try tui_text.appendCell(allocator, &raw, text, layout.content_width);
    const used_width = layout.content_x + layout.content_width;
    pad = used_width;
    while (pad < layout.width - 1) : (pad += 1) try raw.append(allocator, ' ');
    try raw.append(allocator, '|');
    try appendStyledEditorLine(allocator, out, layout.y + row_offset, layout.x, try raw.toOwnedSlice(allocator), layout.width, ansi, palette, muted);
}

fn commentPreviewStart(body: []const u8, max_rows: usize) usize {
    if (body.len == 0 or max_rows == 0) return 0;
    var rows: usize = 1;
    var i = body.len;
    while (i > 0) {
        i -= 1;
        if (body[i] == '\n') {
            if (rows == max_rows) return i + 1;
            rows += 1;
        }
    }
    return 0;
}

fn commentPreviewStartForCursor(body: []const u8, cursor: usize, max_rows: usize) usize {
    if (body.len == 0 or max_rows == 0) return 0;
    var rows: usize = 1;
    var i = @min(cursor, body.len);
    while (i > 0) {
        i -= 1;
        if (body[i] == '\n') {
            if (rows == max_rows) return i + 1;
            rows += 1;
        }
    }
    return 0;
}

fn writeAll(io: std.Io, bytes: []const u8) !void {
    try std.Io.File.stdout().writeStreamingAll(io, bytes);
}

const SignalCleanup = struct {
    previous_int: ?std.posix.Sigaction = null,
    previous_term: ?std.posix.Sigaction = null,

    var active: bool = false;

    fn install() SignalCleanup {
        if (builtin.os.tag != .linux) return .{};
        var cleanup = SignalCleanup{};
        const action: std.posix.Sigaction = .{
            .handler = .{ .handler = handleSignal },
            .mask = std.posix.sigemptyset(),
            .flags = 0,
        };
        var previous_int: std.posix.Sigaction = undefined;
        std.posix.sigaction(.INT, &action, &previous_int);
        cleanup.previous_int = previous_int;
        var previous_term: std.posix.Sigaction = undefined;
        std.posix.sigaction(.TERM, &action, &previous_term);
        cleanup.previous_term = previous_term;
        return cleanup;
    }

    fn restore(self: *SignalCleanup) void {
        if (self.previous_int) |previous| std.posix.sigaction(.INT, &previous, null);
        if (self.previous_term) |previous| std.posix.sigaction(.TERM, &previous, null);
    }

    fn handleSignal(signal: std.posix.SIG) callconv(.c) void {
        if (active) {
            _ = std.os.linux.write(std.posix.STDOUT_FILENO, disable_extended_keyboard.ptr, disable_extended_keyboard.len);
            _ = std.os.linux.write(std.posix.STDOUT_FILENO, leave_tui_screen.ptr, leave_tui_screen.len);
        }
        const status: i32 = switch (signal) {
            .INT => 130,
            .TERM => 143,
            else => 128,
        };
        std.os.linux.exit(status);
    }
};

const RawTerminal = struct {
    original: std.posix.termios,

    fn enter() !RawTerminal {
        const original = try std.posix.tcgetattr(std.posix.STDIN_FILENO);
        var raw = original;
        raw.lflag.ICANON = false;
        raw.lflag.ECHO = false;
        raw.lflag.IEXTEN = false;
        raw.lflag.ISIG = false;
        raw.cc[@intFromEnum(std.posix.V.MIN)] = 1;
        raw.cc[@intFromEnum(std.posix.V.TIME)] = 0;
        try std.posix.tcsetattr(std.posix.STDIN_FILENO, .FLUSH, raw);
        return .{ .original = original };
    }

    fn restore(self: *RawTerminal) void {
        std.posix.tcsetattr(std.posix.STDIN_FILENO, .FLUSH, self.original) catch {};
    }
};

test "parse sgr mouse wheel" {
    const event = parseSgrMouse("\x1b[<65;10;5M").?;
    try std.testing.expect(event.isWheelDown());
    try std.testing.expectEqual(@as(usize, 10), event.x);
    try std.testing.expectEqual(@as(usize, 5), event.y);
}

test "parse sgr mouse left drag" {
    const event = parseSgrMouse("\x1b[<32;12;8M").?;
    try std.testing.expect(event.isLeftDrag());
    try std.testing.expectEqual(@as(usize, 12), event.x);
    try std.testing.expectEqual(@as(usize, 8), event.y);
}

test "layout body fills rows between status and footer" {
    const layout = Layout.init(.{ .width = 100, .height = 20 });
    try std.testing.expectEqual(@as(usize, 18), layout.body_height);
}

test "wrap ansi text ignores color escapes in visible width" {
    const allocator = std.testing.allocator;
    const rows = try wrapAnsiText(allocator, "\x1b[31mabcdef\x1b[0m", 3, 4);
    defer {
        for (rows) |row| allocator.free(row);
        allocator.free(rows);
    }
    try std.testing.expectEqual(@as(usize, 2), rows.len);
    try std.testing.expectEqualStrings("\x1b[31mabc", rows[0]);
    try std.testing.expectEqualStrings("def\x1b[0m", rows[1]);
}

test "inline range background survives syntax resets" {
    const allocator = std.testing.allocator;
    const ansi = theme.Ansi{ .enabled = true, .true_color = true };
    const base_bg: theme.Color = .{ .hex = "#1e3a2f" };
    const inline_bg: theme.Color = .{ .hex = "#2f6f4f" };
    const inline_code = try ansi.bg(allocator, inline_bg);
    defer allocator.free(inline_code);

    const rendered = try applyInlineRanges(
        allocator,
        "\x1b[38;2;1;2;3mold\x1b[0mValue",
        "oldValue",
        &.{.{ .start = 0, .end = 8 }},
        ansi,
        base_bg,
        inline_bg,
    );
    defer allocator.free(rendered);

    const first = std.mem.indexOf(u8, rendered, inline_code) orelse return error.TestExpectedEqual;
    const last = std.mem.lastIndexOf(u8, rendered, inline_code) orelse return error.TestExpectedEqual;
    try std.testing.expect(last > first);
}

test "wrap ansi text supports narrower continuation rows" {
    const allocator = std.testing.allocator;
    const rows = try wrapAnsiTextWithWidths(allocator, "abcdefghi", 4, 3, 4);
    defer {
        for (rows) |row| allocator.free(row);
        allocator.free(rows);
    }
    try std.testing.expectEqual(@as(usize, 3), rows.len);
    try std.testing.expectEqualStrings("abcd", rows[0]);
    try std.testing.expectEqualStrings("efg", rows[1]);
    try std.testing.expectEqualStrings("hi", rows[2]);
}

test "wrap ansi text prefers word boundaries" {
    const allocator = std.testing.allocator;
    const rows = try wrapAnsiText(allocator, "alpha beta gamma", 12, 4);
    defer {
        for (rows) |row| allocator.free(row);
        allocator.free(rows);
    }
    try std.testing.expectEqual(@as(usize, 2), rows.len);
    try std.testing.expectEqualStrings("alpha beta", rows[0]);
    try std.testing.expectEqualStrings("gamma", rows[1]);
}

test "wrap ansi text can break at code delimiters" {
    const allocator = std.testing.allocator;
    const rows = try wrapAnsiText(allocator, "call(alpha,beta)", 11, 4);
    defer {
        for (rows) |row| allocator.free(row);
        allocator.free(rows);
    }
    try std.testing.expectEqual(@as(usize, 2), rows.len);
    try std.testing.expectEqualStrings("call(alpha,", rows[0]);
    try std.testing.expectEqualStrings("beta)", rows[1]);
}

test "wrap ansi text can break after assignment" {
    const allocator = std.testing.allocator;
    const rows = try wrapAnsiText(allocator, "const value = call()", 14, 4);
    defer {
        for (rows) |row| allocator.free(row);
        allocator.free(rows);
    }
    try std.testing.expectEqual(@as(usize, 2), rows.len);
    try std.testing.expectEqualStrings("const value =", rows[0]);
    try std.testing.expectEqualStrings("call()", rows[1]);
}

test "continuation prefix follows code indentation plus one" {
    try std.testing.expectEqual(@as(usize, 13), continuationPrefixWidth(80, 8, 4));
    try std.testing.expectEqual(@as(usize, 20), continuationPrefixWidth(21, 8, 40));
}

test "file tree start avoids unsigned underflow at first scroll row" {
    try std.testing.expectEqual(@as(usize, 0), fileTreeStartIndex(20, 0, 5));
    try std.testing.expectEqual(@as(usize, 0), fileTreeStartIndex(20, 3, 5));
    try std.testing.expectEqual(@as(usize, 1), fileTreeStartIndex(20, 4, 5));
}

test "selected text copies stacked rows as patch text" {
    var lines = [_]diff.DiffLine{
        .{ .kind = .context, .old_lineno = 1, .new_lineno = 1, .text = @constCast("keep"), .stable_line_id = @constCast("line_keep") },
        .{ .kind = .delete, .old_lineno = 2, .new_lineno = null, .text = @constCast("old"), .stable_line_id = @constCast("line_old") },
        .{ .kind = .add, .old_lineno = null, .new_lineno = 2, .text = @constCast("new"), .stable_line_id = @constCast("line_new") },
    };
    var hunks = [_]diff.DiffHunk{.{
        .header = @constCast("@@ -1,2 +1,2 @@"),
        .old_start = 1,
        .old_count = 2,
        .new_start = 1,
        .new_count = 2,
        .lines = lines[0..],
    }};
    var files = [_]diff.DiffFile{.{
        .path = @constCast("src/app.zig"),
        .old_path = null,
        .status = .modified,
        .source = .explicit,
        .language = null,
        .is_binary = false,
        .hunks = hunks[0..],
        .patch_fingerprint = @constCast("fingerprint"),
        .patch_text = @constCast(""),
    }};
    const snapshot: diff.DiffSnapshot = .{
        .snapshot_id = @constCast("snapshot"),
        .repository = .{
            .root_path = @constCast("/tmp/repo"),
            .repo_id = @constCast("repo"),
            .current_branch = @constCast("main"),
        },
        .review_target = .{
            .kind = .working_tree,
            .raw_args = &.{},
            .normalized_spec = @constCast("working tree"),
            .target_id = @constCast("target"),
        },
        .files = files[0..],
    };
    var state: State = .{
        .active_file = 0,
        .cursor_row = 3,
        .scroll_row = 0,
        .mode = .stacked,
        .selection_start = 2,
        .copy_notice = .none,
        .help = false,
        .pending_g = false,
        .folds = .empty,
        .syntax_cache = undefined,
    };

    const selected = try selectedText(std.testing.allocator, snapshot, &state);
    defer std.testing.allocator.free(selected.bytes);

    try std.testing.expectEqual(@as(usize, 2), selected.line_count);
    try std.testing.expectEqualStrings("-old\n+new", selected.bytes);
}

test "selected text copies split replacement pair" {
    var lines = [_]diff.DiffLine{
        .{ .kind = .delete, .old_lineno = 1, .new_lineno = null, .text = @constCast("old"), .stable_line_id = @constCast("line_old") },
        .{ .kind = .add, .old_lineno = null, .new_lineno = 1, .text = @constCast("new"), .stable_line_id = @constCast("line_new") },
    };
    var hunks = [_]diff.DiffHunk{.{
        .header = @constCast("@@ -1,1 +1,1 @@"),
        .old_start = 1,
        .old_count = 1,
        .new_start = 1,
        .new_count = 1,
        .lines = lines[0..],
    }};
    var files = [_]diff.DiffFile{.{
        .path = @constCast("src/app.zig"),
        .old_path = null,
        .status = .modified,
        .source = .explicit,
        .language = null,
        .is_binary = false,
        .hunks = hunks[0..],
        .patch_fingerprint = @constCast("fingerprint"),
        .patch_text = @constCast(""),
    }};
    const snapshot: diff.DiffSnapshot = .{
        .snapshot_id = @constCast("snapshot"),
        .repository = .{
            .root_path = @constCast("/tmp/repo"),
            .repo_id = @constCast("repo"),
            .current_branch = @constCast("main"),
        },
        .review_target = .{
            .kind = .working_tree,
            .raw_args = &.{},
            .normalized_spec = @constCast("working tree"),
            .target_id = @constCast("target"),
        },
        .files = files[0..],
    };
    var state: State = .{
        .active_file = 0,
        .cursor_row = 1,
        .scroll_row = 0,
        .mode = .split,
        .selection_start = null,
        .copy_notice = .none,
        .help = false,
        .pending_g = false,
        .folds = .empty,
        .syntax_cache = undefined,
    };

    const selected = try selectedText(std.testing.allocator, snapshot, &state);
    defer std.testing.allocator.free(selected.bytes);

    try std.testing.expectEqual(@as(usize, 2), selected.line_count);
    try std.testing.expectEqualStrings("-old\n+new", selected.bytes);
}

test "goto action tracks gg prefix and G edge jump" {
    try std.testing.expectEqual(GotoAction.pending, gotoAction("g", false));
    try std.testing.expectEqual(GotoAction.top, gotoAction("g", true));
    try std.testing.expectEqual(GotoAction.top, gotoAction("gg", false));
    try std.testing.expectEqual(GotoAction.bottom, gotoAction("G", false));
    try std.testing.expectEqual(GotoAction.none, gotoAction("j", true));
}

test "plain escape is distinct from escape sequences" {
    try std.testing.expect(isPlainEscape("\x1b"));
    try std.testing.expect(!isPlainEscape("\x1b[A"));
    try std.testing.expect(!isPlainEscape("\x1b[6~"));
}

test "cancel selection mode clears selection state" {
    var state: State = .{
        .active_file = 0,
        .cursor_row = 3,
        .scroll_row = 0,
        .mode = .stacked,
        .selection_start = 1,
        .copy_notice = .{ .copied = 2 },
        .help = true,
        .pending_g = true,
        .folds = .empty,
        .syntax_cache = undefined,
    };

    cancelSelectionMode(&state);

    try std.testing.expectEqual(@as(?usize, null), state.selection_start);
    try std.testing.expect(!state.help);
    try std.testing.expect(!state.pending_g);
    try std.testing.expectEqual(CopyNotice.none, state.copy_notice);
}

test "adding comment at cursor persists" {
    const allocator = std.testing.allocator;
    const patch =
        \\diff --git a/a.zig b/a.zig
        \\--- a/a.zig
        \\+++ b/a.zig
        \\@@ -1,2 +1,2 @@
        \\ one
        \\-old();
        \\+new();
        \\
    ;
    const files = try diff.parsePatch(allocator, patch, .explicit);
    defer {
        for (files) |*file| file.deinit(allocator);
        allocator.free(files);
    }
    const snapshot: diff.DiffSnapshot = .{
        .snapshot_id = @constCast("snapshot"),
        .repository = .{
            .root_path = @constCast("/tmp/repo"),
            .repo_id = @constCast("repo"),
            .current_branch = @constCast("main"),
        },
        .review_target = .{
            .kind = .working_tree,
            .raw_args = &.{},
            .normalized_spec = @constCast("working tree"),
            .target_id = @constCast("target"),
        },
        .files = files,
    };
    var state: State = .{
        .active_file = 0,
        .cursor_row = 0,
        .scroll_row = 0,
        .mode = .stacked,
        .selection_start = null,
        .copy_notice = .none,
        .help = false,
        .pending_g = false,
        .folds = .empty,
        .syntax_cache = undefined,
    };
    var view = try currentView(allocator, snapshot, &state);
    defer view.deinit(allocator);
    for (view.rows, 0..) |row, i| {
        if (row.commentLine() != null) {
            state.cursor_row = i;
            break;
        }
    } else return error.TestExpectedEqual;

    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path_z = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);
    const tmp_path: []const u8 = tmp_path_z;
    defer allocator.free(tmp_path_z);
    var store = store_mod.Store{
        .allocator = allocator,
        .io = std.testing.io,
        .repo_dir = try util.dupe(allocator, tmp_path),
        .comments_path = try std.fmt.allocPrint(allocator, "{s}/comments.json", .{tmp_path}),
        .states_path = try std.fmt.allocPrint(allocator, "{s}/review-states.json", .{tmp_path}),
        .comments = try allocator.alloc(store_mod.Comment, 0),
        .states = try allocator.alloc(store_mod.ReviewState, 0),
    };
    defer store.deinit();

    try std.testing.expect(try addCommentAtCursor(allocator, snapshot, &store, &state, "persisted body", "tester"));
    try std.testing.expectEqual(@as(usize, 1), store.comments.len);
    try std.testing.expectEqualStrings("persisted body", store.comments[0].body);

    const comments_json = try std.Io.Dir.cwd().readFileAlloc(std.testing.io, store.comments_path, allocator, .limited(1024 * 1024));
    defer allocator.free(comments_json);
    try std.testing.expect(std.mem.indexOf(u8, comments_json, "persisted body") != null);
}

test "center cursor places target row at viewport midpoint" {
    const rows = [_]tui_view.VisualRow{.{ .kind = .file_header }} ** 30;
    const changes = [_]tui_view.ChangeSpan{};
    const view: tui_view.FileView = .{
        .rows = @constCast(rows[0..]),
        .changes = @constCast(changes[0..]),
        .additions = 0,
        .deletions = 0,
    };
    var files = [_]diff.DiffFile{.{
        .path = @constCast("a.zig"),
        .old_path = null,
        .status = .modified,
        .source = .explicit,
        .language = null,
        .is_binary = false,
        .hunks = @constCast(&[_]diff.DiffHunk{}),
        .patch_fingerprint = @constCast("fingerprint"),
        .patch_text = @constCast(""),
    }};
    const snapshot: diff.DiffSnapshot = .{
        .snapshot_id = @constCast("snapshot"),
        .repository = .{
            .root_path = @constCast("/tmp/repo"),
            .repo_id = @constCast("repo"),
            .current_branch = @constCast("main"),
        },
        .review_target = .{
            .kind = .working_tree,
            .raw_args = &.{},
            .normalized_spec = @constCast("working tree"),
            .target_id = @constCast("target"),
        },
        .files = files[0..],
    };
    var state: State = .{
        .active_file = 0,
        .cursor_row = 20,
        .scroll_row = 0,
        .mode = .stacked,
        .selection_start = null,
        .copy_notice = .none,
        .help = false,
        .pending_g = false,
        .folds = .empty,
        .syntax_cache = undefined,
    };

    try centerCursorInLayout(std.testing.allocator, snapshot, view, &state, Layout.init(.{ .width = 100, .height = 12 }));

    try std.testing.expectEqual(@as(usize, 20), state.cursor_row);
    try std.testing.expectEqual(@as(usize, 15), state.scroll_row);

    state.cursor_row = 28;
    state.scroll_row = 0;
    try centerCursorInLayout(std.testing.allocator, snapshot, view, &state, Layout.init(.{ .width = 100, .height = 12 }));

    try std.testing.expectEqual(@as(usize, 28), state.cursor_row);
    try std.testing.expectEqual(@as(usize, 23), state.scroll_row);
}

test "fold toggle requires fold row and preserves code offset in tall viewport" {
    const allocator = std.testing.allocator;
    var patch: std.ArrayList(u8) = .empty;
    defer patch.deinit(allocator);
    try patch.appendSlice(allocator,
        \\diff --git a/sample.txt b/sample.txt
        \\--- a/sample.txt
        \\+++ b/sample.txt
        \\@@ -1,130 +1,130 @@
        \\
    );
    for (1..131) |line_no| {
        if (line_no == 118) {
            try patch.appendSlice(allocator,
                \\-line 118
                \\+line 118 changed
                \\
            );
        } else {
            const line_text = try std.fmt.allocPrint(allocator, " line {d}\n", .{line_no});
            defer allocator.free(line_text);
            try patch.appendSlice(allocator, line_text);
        }
    }
    const files = try diff.parsePatch(allocator, patch.items, .explicit);
    defer {
        for (files) |*file| file.deinit(allocator);
        allocator.free(files);
    }
    const snapshot: diff.DiffSnapshot = .{
        .snapshot_id = @constCast("snapshot"),
        .repository = .{
            .root_path = @constCast("/tmp/repo"),
            .repo_id = @constCast("repo"),
            .current_branch = @constCast("main"),
        },
        .review_target = .{
            .kind = .working_tree,
            .raw_args = &.{},
            .normalized_spec = @constCast("working tree"),
            .target_id = @constCast("target"),
        },
        .files = files,
    };
    var state: State = .{
        .active_file = 0,
        .cursor_row = 0,
        .scroll_row = 0,
        .mode = .stacked,
        .selection_start = null,
        .copy_notice = .none,
        .help = false,
        .pending_g = false,
        .folds = .empty,
        .syntax_cache = undefined,
    };
    defer state.folds.deinit(allocator);
    const layout = Layout.init(.{ .width = 80, .height = 24 });

    var before = try currentView(allocator, snapshot, &state);
    defer before.deinit(allocator);
    var first_fold_row: ?usize = null;
    var line_115_row: ?usize = null;
    for (before.rows, 0..) |row, i| {
        if (row.kind == .fold and first_fold_row == null) first_fold_row = i;
        if (row.line) |line| {
            if (util.eql(line.text, "line 115")) {
                line_115_row = i;
                break;
            }
        }
    }
    const fold_row = first_fold_row orelse return error.TestExpectedEqual;
    const line_115_before = line_115_row orelse return error.TestExpectedEqual;
    const before_offset = try visualHeightBetween(allocator, snapshot, &state, before, state.scroll_row, line_115_before, layout);

    try toggleCurrentFoldInLayout(allocator, snapshot, &state, layout);

    var after = try currentView(allocator, snapshot, &state);
    defer after.deinit(allocator);
    try ensureRenderViewport(allocator, snapshot, after, &state, layout);
    try std.testing.expectEqual(@as(usize, 0), state.cursor_row);
    try std.testing.expectEqual(@as(usize, 0), state.scroll_row);
    try std.testing.expectEqual(@as(usize, 0), state.folds.items.len);
    try std.testing.expectEqual(tui_view.RowKind.fold, after.rows[fold_row].kind);
    try std.testing.expect(!after.rows[fold_row].fold_expanded);

    var fold_state: State = .{
        .active_file = 0,
        .cursor_row = fold_row,
        .scroll_row = 0,
        .mode = .stacked,
        .selection_start = null,
        .copy_notice = .none,
        .help = false,
        .pending_g = false,
        .folds = .empty,
        .syntax_cache = undefined,
    };
    defer fold_state.folds.deinit(allocator);

    try toggleCurrentFoldInLayout(allocator, snapshot, &fold_state, layout);

    var after_fold = try currentView(allocator, snapshot, &fold_state);
    defer after_fold.deinit(allocator);
    try ensureRenderViewport(allocator, snapshot, after_fold, &fold_state, layout);
    const fold_line = after_fold.rows[fold_state.cursor_row].line orelse return error.TestExpectedEqual;
    try std.testing.expectEqualStrings("line 115", fold_line.text);
    const fold_offset = try visualHeightBetween(allocator, snapshot, &fold_state, after_fold, fold_state.scroll_row, fold_state.cursor_row, layout);
    try std.testing.expectEqual(before_offset, fold_offset);

    var all_state: State = .{
        .active_file = 0,
        .cursor_row = 0,
        .scroll_row = 0,
        .mode = .stacked,
        .selection_start = null,
        .copy_notice = .none,
        .help = false,
        .pending_g = false,
        .folds = .empty,
        .syntax_cache = undefined,
    };
    defer all_state.folds.deinit(allocator);

    try toggleAllFoldsInLayout(allocator, snapshot, &all_state, layout);

    var after_all = try currentView(allocator, snapshot, &all_state);
    defer after_all.deinit(allocator);
    try ensureRenderViewport(allocator, snapshot, after_all, &all_state, layout);
    const all_line = after_all.rows[all_state.cursor_row].line orelse return error.TestExpectedEqual;
    try std.testing.expectEqualStrings("line 115", all_line.text);
    const all_offset = try visualHeightBetween(allocator, snapshot, &all_state, after_all, all_state.scroll_row, all_state.cursor_row, layout);
    try std.testing.expectEqual(before_offset, all_offset);
}

test "comment input backspace removes utf8 character" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);
    try input.appendSlice(allocator, "ab架");
    input.backspace();
    try std.testing.expectEqualStrings("ab", input.body.items);
    try std.testing.expectEqual(@as(usize, 2), input.cursor);
}

test "comment input preserves multiline body" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);
    try input.appendSlice(allocator, "first");
    try input.appendNewline(allocator);
    try input.appendSlice(allocator, "second");
    try std.testing.expectEqualStrings("first\nsecond", input.body.items);
}

test "comment input normalizes crlf and handles buffered backspace" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);

    try std.testing.expectEqual(.continue_input, try handleCommentInputBytes(allocator, &input, "first\r\nsecond"));
    try std.testing.expectEqualStrings("first\nsecond", input.body.items);
    try std.testing.expectEqual(.continue_input, try handleCommentInputBytes(allocator, &input, "\x7f!"));
    try std.testing.expectEqualStrings("first\nsecon!", input.body.items);
}

test "comment input treats delete escape as a delete byte" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);

    try input.appendSlice(allocator, "abc");
    input.moveLeft();
    try std.testing.expectEqual(.continue_input, try handleCommentInputBytes(allocator, &input, "\x1b[3~"));
    try std.testing.expectEqualStrings("ab", input.body.items);
    try std.testing.expectEqual(@as(usize, 2), input.cursor);
}

test "comment input saves with ctrl-enter and cancels with escape" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);

    try std.testing.expectEqual(.save, try handleCommentInputBytes(allocator, &input, "body\x1b[13;5uignored"));
    try std.testing.expectEqualStrings("body", input.body.items);
    try std.testing.expectEqual(.cancel, try handleCommentInputBytes(allocator, &input, "\x1b"));
}

test "comment input accepts common ctrl-enter encodings" {
    const allocator = std.testing.allocator;
    const sequences = [_][]const u8{
        "\x1b[13;5u",
        "\x1b[10;5u",
        "\x1b[13;5~",
        "\x1b[10;5~",
        "\x1b[27;5;13~",
        "\x1b[27;5;10~",
    };

    for (sequences) |sequence| {
        var input = CommentInput{};
        defer input.deinit(allocator);
        try std.testing.expectEqual(.save, try handleCommentInputBytes(allocator, &input, sequence));
    }
}

test "comment input buffers split ctrl-enter escape" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);

    const first = try handleCommentInputBytesPartial(allocator, &input, "body\x1b[13;");
    try std.testing.expectEqual(CommentInputAction.incomplete_escape, first.action);
    try std.testing.expectEqual(@as(usize, 4), first.consumed);
    try std.testing.expectEqualStrings("body", input.body.items);

    var pending: std.ArrayList(u8) = .empty;
    defer pending.deinit(allocator);
    try pending.appendSlice(allocator, "body\x1b[13;");
    discardCommentInputPrefix(&pending, first.consumed);
    try pending.appendSlice(allocator, "5u");
    const second = try handleCommentInputBytesPartial(allocator, &input, pending.items);
    try std.testing.expectEqual(CommentInputAction.save, second.action);
}

test "comment input ignores ctrl-d as an editing byte" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);

    try input.appendSlice(allocator, "body");
    try std.testing.expectEqual(.continue_input, try handleCommentInputBytes(allocator, &input, "\x04"));
    try std.testing.expectEqualStrings("body", input.body.items);
}

test "comment input treats ctrl-c as quit" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);

    try input.appendSlice(allocator, "body");
    try std.testing.expectEqual(.quit, try handleCommentInputBytes(allocator, &input, "\x03"));
    try std.testing.expectEqualStrings("body", input.body.items);
}

test "comment input arrows move cursor and insert at cursor" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);

    try std.testing.expectEqual(.continue_input, try handleCommentInputBytes(allocator, &input, "abcd"));
    try std.testing.expectEqual(.continue_input, try handleCommentInputBytes(allocator, &input, "\x1b[D\x1b[D"));
    try input.appendByte(allocator, 'X');
    try std.testing.expectEqualStrings("abXcd", input.body.items);
    try std.testing.expectEqual(@as(usize, 3), input.cursor);
    try std.testing.expectEqual(.continue_input, try handleCommentInputBytes(allocator, &input, "\x1b[C"));
    input.backspace();
    try std.testing.expectEqualStrings("abXd", input.body.items);
}

test "comment input vertical arrows preserve display column" {
    const allocator = std.testing.allocator;
    var input = CommentInput{};
    defer input.deinit(allocator);

    try input.appendSlice(allocator, "abc\n架de\nxy");
    input.cursor = 7;
    input.moveUp();
    try std.testing.expectEqual(@as(usize, 2), input.cursor);
    input.moveDown();
    try std.testing.expectEqual(@as(usize, 7), input.cursor);
    input.moveDown();
    try std.testing.expectEqual(@as(usize, 12), input.cursor);
}

test "comment editor layout floats within terminal" {
    const layout = CommentEditorLayout.init(.{ .width = 120, .height = 40 });
    try std.testing.expectEqual(@as(usize, 11), layout.panel_x);
    try std.testing.expectEqual(@as(usize, 13), layout.panel_y);
    try std.testing.expectEqual(@as(usize, 100), layout.panel_width);
    try std.testing.expectEqual(@as(usize, 15), layout.panel_height);
    try std.testing.expectEqual(@as(usize, 19), layout.x);
    try std.testing.expectEqual(@as(usize, 14), layout.y);
    try std.testing.expectEqual(@as(usize, 84), layout.width);
    try std.testing.expectEqual(@as(usize, 14), layout.height);
    try std.testing.expectEqual(@as(usize, 3), layout.content_x);
    try std.testing.expectEqual(@as(usize, 79), layout.content_width);
    try std.testing.expectEqual(@as(usize, 2), layout.body_y);
    try std.testing.expectEqual(@as(usize, 10), layout.body_rows);
}

test "comment cursor follows visible body tail" {
    const layout: CommentEditorLayout = .{
        .panel_x = 8,
        .panel_y = 3,
        .panel_width = 44,
        .panel_height = 10,
        .x = 10,
        .y = 4,
        .width = 40,
        .height = 8,
        .content_x = 3,
        .content_width = 35,
        .body_y = 2,
        .body_rows = 4,
    };

    var pos = commentCursorPosition("", 0, 0, layout);
    try std.testing.expectEqual(@as(usize, 13), pos.x);
    try std.testing.expectEqual(@as(usize, 6), pos.y);

    pos = commentCursorPosition("one\n架", "one\n架".len, 0, layout);
    try std.testing.expectEqual(@as(usize, 15), pos.x);
    try std.testing.expectEqual(@as(usize, 7), pos.y);

    const body = "one\ntwo\nthree";
    const start = commentPreviewStartForCursor(body, body.len, 2);
    pos = commentCursorPosition(body, body.len, start, .{
        .panel_x = layout.panel_x,
        .panel_y = layout.panel_y,
        .panel_width = layout.panel_width,
        .panel_height = layout.panel_height,
        .x = layout.x,
        .y = layout.y,
        .width = layout.width,
        .height = 6,
        .content_x = layout.content_x,
        .content_width = layout.content_width,
        .body_y = layout.body_y,
        .body_rows = 2,
    });
    try std.testing.expectEqual(@as(usize, 18), pos.x);
    try std.testing.expectEqual(@as(usize, 7), pos.y);
}

test "comment editor line keeps declared cell width with cjk text" {
    const allocator = std.testing.allocator;
    const layout: CommentEditorLayout = .{
        .panel_x = 1,
        .panel_y = 1,
        .panel_width = 10,
        .panel_height = 4,
        .x = 1,
        .y = 1,
        .width = 10,
        .height = 4,
        .content_x = 3,
        .content_width = 5,
        .body_y = 1,
        .body_rows = 1,
    };

    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    try appendEditorLine(allocator, &out, layout, 1, "架ab", .{ .enabled = false, .true_color = false }, theme.catppuccinMocha(), false);
    const line_start = (std.mem.indexOfScalar(u8, out.items, 'H') orelse return error.TestExpectedEqual) + 1;
    try std.testing.expectEqual(@as(usize, 10), displayWidthRange(out.items, line_start, out.items.len));
}

test "comment preview starts at visible trailing line" {
    try std.testing.expectEqual(@as(usize, 0), commentPreviewStart("one\ntwo", 2));
    try std.testing.expectEqual(@as(usize, 4), commentPreviewStart("one\ntwo\nthree", 2));
}

test "fold target only matches the current fold row" {
    const fold_id: tui_view.FoldId = .{ .file_index = 0, .hunk_index = 0, .ordinal = 0 };
    const rows = [_]tui_view.VisualRow{
        .{ .kind = .file_header },
        .{ .kind = .hunk_header },
        .{ .kind = .stacked_code },
        .{ .kind = .fold, .fold_id = fold_id, .fold_line_count = 10 },
        .{ .kind = .stacked_code, .fold_id = fold_id },
    };
    const changes = [_]tui_view.ChangeSpan{};
    const view: tui_view.FileView = .{
        .rows = @constCast(rows[0..]),
        .changes = @constCast(changes[0..]),
        .additions = 0,
        .deletions = 0,
    };
    try std.testing.expectEqual(@as(?usize, null), findFoldTarget(view, 0));
    try std.testing.expectEqual(@as(?usize, null), findFoldTarget(view, 2));
    try std.testing.expectEqual(@as(?usize, 3), findFoldTarget(view, 3));
    try std.testing.expectEqual(@as(?usize, null), findFoldTarget(view, 4));
}
