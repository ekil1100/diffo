const std = @import("std");
const builtin = @import("builtin");
const diff = @import("diff.zig");
const store_mod = @import("store.zig");
const syntax = @import("syntax.zig");
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
    help: bool = false,
    folds: std.ArrayList(tui_view.FoldEntry) = .empty,

    fn deinit(self: *State, allocator: std.mem.Allocator) void {
        self.folds.deinit(allocator);
    }
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
            .body_height = height - 3,
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

    var state = State{};
    defer state.deinit(allocator);
    const is_tty = (std.Io.File.stdout().isTty(io) catch false) and (std.Io.File.stdin().isTty(io) catch false);
    if (!is_tty) {
        const screen = try render(allocator, snapshot.*, store.*, state, false);
        defer allocator.free(screen);
        try writeAll(io, screen);
        return;
    }

    var raw = RawTerminal.enter() catch null;
    defer if (raw) |*term| term.restore();

    try writeAll(io, "\x1b[?1049h\x1b[2J\x1b[H\x1b[?25l\x1b[?1000h\x1b[?1002h\x1b[?1006h");
    defer writeAll(io, "\x1b[?1006l\x1b[?1002l\x1b[?1000l\x1b[?25h\x1b[?1049l") catch {};

    while (true) {
        const screen = try render(allocator, snapshot.*, store.*, state, true);
        defer allocator.free(screen);
        try writeAll(io, screen);

        const event = readEvent() orelse continue;
        if (try handleEvent(allocator, io, snapshot, store, author, &state, event, &raw)) break;
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
    raw: *?RawTerminal,
) !bool {
    switch (event) {
        .mouse => |mouse| {
            try handleMouse(allocator, snapshot.*, state, mouse);
            return false;
        },
        .key => |key| {
            if (key.len == 0) return false;
            if (key[0] == 'q') return true;
            if (state.help and key[0] != '?') {
                state.help = false;
                return false;
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
                'u' => jumpUnreviewed(snapshot.*, store.*, state),
                'r' => {
                    const file = snapshot.files[state.active_file];
                    const reviewed = !store.isReviewed(file.path, file.patch_fingerprint, snapshot.review_target.target_id);
                    try store.setReviewed(snapshot.repository.repo_id, snapshot.review_target.target_id, file, reviewed);
                },
                'c' => {
                    if (raw.*) |*term| term.restore();
                    try writeAll(io, "\x1b[?25h\x1b[2K\rcomment: ");
                    const body = try readLine(allocator);
                    defer allocator.free(body);
                    if (body.len > 0) try addCommentAtCursor(allocator, snapshot.*, store, state.*, body, author);
                    raw.* = RawTerminal.enter() catch null;
                    try writeAll(io, "\x1b[?25l");
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
    if (!mouse.isLeftPress()) return;
    if (mouse.y < layout.body_y or mouse.y >= layout.footer_y) return;

    if (in_sidebar) {
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
        state.cursor_row = @min(state.scroll_row + mouse.y - layout.body_y, total - 1);
        ensureCursorVisible(view, state);
    }
}

fn render(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    store: store_mod.Store,
    state: State,
    colors: bool,
) ![]u8 {
    const layout = Layout.init(terminalSize());
    const palette = theme.catppuccinMocha();
    const ansi = theme.Ansi.init(colors);
    const clear_eol = if (colors) "\x1b[K" else "";
    var view = try currentView(allocator, snapshot, &state);
    defer view.deinit(allocator);

    var out: std.ArrayList(u8) = .empty;
    if (colors) try out.appendSlice(allocator, "\x1b[?2026h\x1b[H");
    try appendStatus(allocator, &out, snapshot, store, state, layout.width);
    try out.appendSlice(allocator, clear_eol);
    try out.append(allocator, '\n');

    const diff_lines = try renderDiffLines(allocator, snapshot.files[state.active_file], view, state, layout.main_width, layout.body_height, ansi, palette);
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
        try tui_text.appendCell(allocator, &out, "j/k arrows move  PgUp/PgDn scroll  J/K file  n/N change  z/Z fold  v view  r reviewed  c comment  V select  u unreviewed  ? help  q quit", layout.width);
    } else {
        const status = syntax.registryStatus();
        const footer = try std.fmt.allocPrint(allocator, "mode={s} target={s} syntax={s}  ? help  n/N change  z fold  q quit", .{ state.mode.label(), snapshot.review_target.normalized_spec, @tagName(status.mode) });
        defer allocator.free(footer);
        try tui_text.appendCell(allocator, &out, footer, layout.width);
    }
    if (colors) try out.appendSlice(allocator, "\x1b[K\x1b[?2026l");
    return out.toOwnedSlice(allocator);
}

fn appendStatus(allocator: std.mem.Allocator, out: *std.ArrayList(u8), snapshot: diff.DiffSnapshot, store: store_mod.Store, state: State, width: usize) !void {
    var reviewed: usize = 0;
    for (snapshot.files) |file| {
        if (store.isReviewed(file.path, file.patch_fingerprint, snapshot.review_target.target_id)) reviewed += 1;
    }
    const file = snapshot.files[state.active_file];
    const line = try std.fmt.allocPrint(allocator, " diffo  {s}  {s}  files {d}/{d} reviewed  file {d}/{d}  {s}", .{ snapshot.repository.current_branch, snapshot.review_target.normalized_spec, reviewed, snapshot.files.len, state.active_file + 1, snapshot.files.len, file.path });
    defer allocator.free(line);
    try tui_text.appendCell(allocator, out, line, width);
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
    file: diff.DiffFile,
    view: tui_view.FileView,
    state: State,
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
        const rendered = try renderVisualRow(allocator, file, view, view.rows[row_index], row_index, state, width, line_width, ansi, palette);
        try lines.append(allocator, rendered);
    }
    return lines.toOwnedSlice(allocator);
}

fn renderVisualRow(
    allocator: std.mem.Allocator,
    file: diff.DiffFile,
    view: tui_view.FileView,
    row: tui_view.VisualRow,
    row_index: usize,
    state: State,
    width: usize,
    line_width: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    const selected = isSelected(state, row_index);
    switch (row.kind) {
        .file_header => return renderFileHeader(allocator, file, view, width, ansi, palette),
        .hunk_header => return renderPanelRow(allocator, row.hunk_header orelse "", width, selected, ansi, palette),
        .fold => return renderFoldRow(allocator, row, width, selected, ansi, palette),
        .stacked_code, .file_meta => {
            const line = row.line orelse return renderPanelRow(allocator, "", width, selected, ansi, palette);
            return renderStackedCodeRow(allocator, file, line, width, line_width, selected, ansi, palette);
        },
        .split_code => return renderSplitCodeRow(allocator, file, row, width, line_width, selected, ansi, palette),
    }
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
    file: diff.DiffFile,
    line: *const diff.DiffLine,
    width: usize,
    line_width: usize,
    selected: bool,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    const label = try lineLabel(allocator, line, line_width);
    defer allocator.free(label);
    const highlighted = try syntax.highlightLine(allocator, ansi, palette, file.language, line.text);
    defer allocator.free(highlighted);
    const bar = switch (line.kind) {
        .add => "|",
        .delete => "|",
        else => " ",
    };
    const raw = try std.fmt.allocPrint(allocator, "{s} {s}  {s}", .{ bar, label, highlighted });
    defer allocator.free(raw);
    return styleCell(allocator, raw, width, ansi, rowBg(selected, line.kind, palette), rowFg(line.kind, palette));
}

fn renderSplitCodeRow(
    allocator: std.mem.Allocator,
    file: diff.DiffFile,
    row: tui_view.VisualRow,
    width: usize,
    line_width: usize,
    selected: bool,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    if (width < 32) {
        if (row.right) |right| return renderStackedCodeRow(allocator, file, right, width, line_width, selected, ansi, palette);
        if (row.left) |left| return renderStackedCodeRow(allocator, file, left, width, line_width, selected, ansi, palette);
        return renderPanelRow(allocator, "", width, selected, ansi, palette);
    }

    const separator_width: usize = 3;
    const side_width = (width - separator_width) / 2;
    const left = try renderSplitCell(allocator, file, row.left, side_width, line_width, selected, ansi, palette);
    defer allocator.free(left);
    const right = try renderSplitCell(allocator, file, row.right, width - side_width - separator_width, line_width, selected, ansi, palette);
    defer allocator.free(right);
    return std.fmt.allocPrint(allocator, "{s}{s} │ {s}{s}", .{ left, ansi.reset(), right, ansi.reset() });
}

fn renderSplitCell(
    allocator: std.mem.Allocator,
    file: diff.DiffFile,
    maybe_line: ?*const diff.DiffLine,
    width: usize,
    line_width: usize,
    selected: bool,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    if (maybe_line) |line| {
        const label = try lineLabel(allocator, line, line_width);
        defer allocator.free(label);
        const highlighted = try syntax.highlightLine(allocator, ansi, palette, file.language, line.text);
        defer allocator.free(highlighted);
        const raw = try std.fmt.allocPrint(allocator, "{s}  {s}", .{ label, highlighted });
        defer allocator.free(raw);
        return styleCell(allocator, raw, width, ansi, rowBg(selected, line.kind, palette), rowFg(line.kind, palette));
    }
    return styleCell(allocator, " ", width, ansi, if (selected) palette.bg_selected else palette.bg_default, palette.fg_muted);
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
    if (selected) return palette.bg_selected;
    return switch (kind) {
        .add => palette.diff_add_bg,
        .delete => palette.diff_del_bg,
        else => palette.bg_default,
    };
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

fn isSelected(state: State, row_index: usize) bool {
    if (row_index == state.cursor_row) return true;
    if (state.selection_start) |start| {
        return row_index >= @min(start, state.cursor_row) and row_index <= @max(start, state.cursor_row);
    }
    return false;
}

fn renderFileTree(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    store: store_mod.Store,
    state: State,
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
    const start = fileTreeStart(snapshot, &state, height);
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
    ensureCursorVisible(view, state);
}

fn scrollLines(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, delta: isize) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    const total = view.rows.len;
    if (total == 0) return;
    state.cursor_row = moveIndex(state.cursor_row, total, delta);
    state.scroll_row = moveIndex(state.scroll_row, total, delta);
    ensureCursorVisible(view, state);
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

fn ensureCursorVisible(view: tui_view.FileView, state: *State) void {
    const layout = Layout.init(terminalSize());
    const total = view.rows.len;
    if (total == 0) {
        state.scroll_row = 0;
        return;
    }
    if (state.cursor_row < state.scroll_row) state.scroll_row = state.cursor_row;
    if (state.cursor_row >= state.scroll_row + layout.body_height) {
        state.scroll_row = state.cursor_row - layout.body_height + 1;
    }
    if (state.scroll_row >= total) state.scroll_row = total - 1;
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
    ensureCursorVisible(view, state);
}

fn jumpChange(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State, delta: isize) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    const next = if (delta > 0) tui_view.nextChange(view, state.cursor_row) else tui_view.previousChange(view, state.cursor_row);
    if (next) |row| {
        state.cursor_row = row;
        ensureCursorVisible(view, state);
    }
}

fn toggleCurrentFold(allocator: std.mem.Allocator, snapshot: diff.DiffSnapshot, state: *State) !void {
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
    if (state.cursor_row >= view.rows.len) return;
    const target = findFoldTarget(view, state.cursor_row) orelse return;
    const id = view.rows[target].fold_id orelse return;
    try toggleFold(state, allocator, id);
    var updated = try currentView(allocator, snapshot, state);
    defer updated.deinit(allocator);
    if (findFoldRowById(updated, id)) |row| state.cursor_row = row else state.cursor_row = @min(target, if (updated.rows.len == 0) 0 else updated.rows.len - 1);
    ensureCursorVisible(updated, state);
}

fn findFoldTarget(view: tui_view.FileView, cursor_row: usize) ?usize {
    if (view.rows.len == 0) return null;
    const start = @min(cursor_row, view.rows.len - 1);
    if (view.rows[start].fold_id != null) return start;
    var i = start + 1;
    while (i < view.rows.len) : (i += 1) {
        if (view.rows[i].kind == .fold) return i;
    }
    i = start;
    while (i > 0) {
        i -= 1;
        if (view.rows[i].kind == .fold) return i;
    }
    return null;
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
    var view = try currentView(allocator, snapshot, state);
    defer view.deinit(allocator);
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
    state: State,
    body: []const u8,
    author: []const u8,
) !void {
    const file = snapshot.files[state.active_file];
    var view = try tui_view.buildFileView(allocator, &file, state.active_file, state.mode, state.folds.items);
    defer view.deinit(allocator);
    if (state.cursor_row >= view.rows.len) return;
    const row = view.rows[state.cursor_row];
    const line = row.commentLine() orelse return;
    const hunk_header = if (row.hunk_index) |idx| file.hunks[idx].header else "";
    var end_line: u32 = 0;
    if (state.selection_start) |start| {
        const last_row_index = @min(@max(start, state.cursor_row), view.rows.len - 1);
        if (view.rows[last_row_index].commentLine()) |last| end_line = if (last.kind == .delete) (last.old_lineno orelse 0) else (last.new_lineno orelse last.old_lineno orelse 0);
    }
    var comment = try store.addComment(snapshot.repository.repo_id, snapshot.review_target.target_id, file, line, hunk_header, end_line, body, author);
    defer comment.deinit(allocator);
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
    return .{ .width = terminalEnv("COLUMNS", 100), .height = terminalEnv("LINES", 32) };
}

fn terminalEnv(comptime name: []const u8, fallback: usize) usize {
    const value = (util.envOwned(std.heap.page_allocator, name) catch null) orelse return fallback;
    defer std.heap.page_allocator.free(value);
    return std.fmt.parseInt(usize, value, 10) catch fallback;
}

fn readLine(allocator: std.mem.Allocator) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    while (true) {
        var byte: [1]u8 = undefined;
        const n = try std.posix.read(std.posix.STDIN_FILENO, &byte);
        if (n == 0 or byte[0] == '\n' or byte[0] == '\r') break;
        if (byte[0] == 127 or byte[0] == 8) {
            if (out.items.len > 0) out.items.len -= 1;
            continue;
        }
        try out.append(allocator, byte[0]);
    }
    return out.toOwnedSlice(allocator);
}

fn writeAll(io: std.Io, bytes: []const u8) !void {
    try std.Io.File.stdout().writeStreamingAll(io, bytes);
}

const RawTerminal = struct {
    original: std.posix.termios,

    fn enter() !RawTerminal {
        const original = try std.posix.tcgetattr(std.posix.STDIN_FILENO);
        var raw = original;
        raw.lflag.ICANON = false;
        raw.lflag.ECHO = false;
        raw.lflag.IEXTEN = false;
        raw.lflag.ISIG = true;
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

test "file tree start avoids unsigned underflow at first scroll row" {
    try std.testing.expectEqual(@as(usize, 0), fileTreeStartIndex(20, 0, 5));
    try std.testing.expectEqual(@as(usize, 0), fileTreeStartIndex(20, 3, 5));
    try std.testing.expectEqual(@as(usize, 1), fileTreeStartIndex(20, 4, 5));
}

test "fold target falls back to nearby fold row" {
    const fold_id: tui_view.FoldId = .{ .file_index = 0, .hunk_index = 0, .ordinal = 0 };
    const rows = [_]tui_view.VisualRow{
        .{ .kind = .file_header },
        .{ .kind = .hunk_header },
        .{ .kind = .stacked_code },
        .{ .kind = .fold, .fold_id = fold_id, .fold_line_count = 10 },
    };
    const changes = [_]tui_view.ChangeSpan{};
    const view: tui_view.FileView = .{
        .rows = @constCast(rows[0..]),
        .changes = @constCast(changes[0..]),
        .additions = 0,
        .deletions = 0,
    };
    try std.testing.expectEqual(@as(?usize, 3), findFoldTarget(view, 0));
    try std.testing.expectEqual(@as(?usize, 3), findFoldTarget(view, 2));
}
