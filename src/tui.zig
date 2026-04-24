const std = @import("std");
const builtin = @import("builtin");
const diff = @import("diff.zig");
const store_mod = @import("store.zig");
const syntax = @import("syntax.zig");
const theme = @import("theme.zig");
const util = @import("util.zig");

pub const DiffMode = enum { inline_view, split };

const State = struct {
    active_file: usize = 0,
    cursor_line: usize = 0,
    scroll: usize = 0,
    mode: DiffMode = .inline_view,
    selection_start: ?usize = null,
    help: bool = false,
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
            handleMouse(snapshot.*, state, mouse);
            return false;
        },
        .key => |key| {
            if (key.len == 0) return false;
            if (key[0] == 'q') return true;
            if (state.help and key[0] != '?') {
                state.help = false;
                return false;
            }

            if (std.mem.eql(u8, key, "\x1b[A")) return keyMove(snapshot.*, state, -1);
            if (std.mem.eql(u8, key, "\x1b[B")) return keyMove(snapshot.*, state, 1);
            if (std.mem.eql(u8, key, "\x1b[5~")) return keyScroll(snapshot.*, state, -12);
            if (std.mem.eql(u8, key, "\x1b[6~")) return keyScroll(snapshot.*, state, 12);

            switch (key[0]) {
                'j' => moveLine(snapshot.*, state, 1),
                'k' => moveLine(snapshot.*, state, -1),
                'J' => moveFile(snapshot.*, state, 1),
                'K' => moveFile(snapshot.*, state, -1),
                'v' => state.mode = if (state.mode == .inline_view) .split else .inline_view,
                '?' => state.help = !state.help,
                'V' => state.selection_start = state.cursor_line,
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

fn keyMove(snapshot: diff.DiffSnapshot, state: *State, delta: isize) bool {
    moveLine(snapshot, state, delta);
    return false;
}

fn keyScroll(snapshot: diff.DiffSnapshot, state: *State, delta: isize) bool {
    scrollLines(snapshot, state, delta);
    return false;
}

fn handleMouse(snapshot: diff.DiffSnapshot, state: *State, mouse: MouseEvent) void {
    const layout = Layout.init(terminalSize());
    const in_sidebar = layout.sidebar_width > 0 and mouse.x >= layout.sidebar_x;
    if (mouse.isWheelUp()) {
        if (in_sidebar) moveFile(snapshot, state, -3) else scrollLines(snapshot, state, -3);
        return;
    }
    if (mouse.isWheelDown()) {
        if (in_sidebar) moveFile(snapshot, state, 3) else scrollLines(snapshot, state, 3);
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
            state.cursor_line = 0;
            state.scroll = 0;
            state.selection_start = null;
        }
    } else {
        const total = snapshot.files[state.active_file].lineCount();
        if (total == 0) return;
        state.cursor_line = @min(state.scroll + mouse.y - layout.body_y, total - 1);
        ensureCursorVisible(snapshot, state);
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

    var out: std.ArrayList(u8) = .empty;
    if (colors) try out.appendSlice(allocator, "\x1b[?2026h\x1b[H");
    try appendStatus(allocator, &out, snapshot, store, state, layout.width);
    try out.appendSlice(allocator, clear_eol);
    try out.append(allocator, '\n');

    const diff_lines = try renderDiffLines(allocator, snapshot.files[state.active_file], state, layout.main_width, layout.body_height, ansi, palette);
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
        try appendCell(allocator, &out, left, layout.main_width);
        if (layout.sidebar_width > 0) {
            try out.appendSlice(allocator, "│");
            const right = if (i < tree_lines.len) tree_lines[i] else "";
            try appendCell(allocator, &out, right, layout.sidebar_width);
        }
        try out.appendSlice(allocator, clear_eol);
        try out.append(allocator, '\n');
    }

    if (state.help) {
        try appendCell(allocator, &out, "j/k or wheel scroll  J/K file  click file tree  v view  r reviewed  c comment  V select  u unreviewed  ? help  q quit", layout.width);
    } else {
        const status = syntax.registryStatus();
        const footer = try std.fmt.allocPrint(allocator, "mode={s} target={s} syntax={s} mouse=on", .{ @tagName(state.mode), snapshot.review_target.normalized_spec, @tagName(status.mode) });
        defer allocator.free(footer);
        try appendCell(allocator, &out, footer, layout.width);
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
    try appendCell(allocator, out, line, width);
}

fn renderDiffLines(
    allocator: std.mem.Allocator,
    file: diff.DiffFile,
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
    const total = file.lineCount();
    const start = @min(state.scroll, total);
    const end = @min(total, start + height);
    for (start..end) |flat| {
        const rendered = try renderFlatLine(allocator, file, flat, state, width, ansi, palette);
        try lines.append(allocator, rendered);
    }
    return lines.toOwnedSlice(allocator);
}

fn renderFlatLine(
    allocator: std.mem.Allocator,
    file: diff.DiffFile,
    flat: usize,
    state: State,
    width: usize,
    ansi: theme.Ansi,
    palette: theme.ThemeTokens,
) ![]u8 {
    const info = diff.findLineByFlatIndex(file, flat) orelse return util.dupe(allocator, "");
    const cursor = if (flat == state.cursor_line) ">" else " ";
    const selected = if (state.selection_start) |start| flat >= @min(start, state.cursor_line) and flat <= @max(start, state.cursor_line) else false;
    if (info.line == null) {
        return std.fmt.allocPrint(allocator, "{s} {s}", .{ cursor, info.header });
    }

    const line = info.line.?;
    const sign: u8 = switch (line.kind) {
        .add => '+',
        .delete => '-',
        .context => ' ',
        .meta => '\\',
    };
    const old_label = if (line.old_lineno) |n| try std.fmt.allocPrint(allocator, "{d: >4}", .{n}) else try util.dupe(allocator, "    ");
    defer allocator.free(old_label);
    const new_label = if (line.new_lineno) |n| try std.fmt.allocPrint(allocator, "{d: >4}", .{n}) else try util.dupe(allocator, "    ");
    defer allocator.free(new_label);
    const highlighted = try syntax.highlightLine(allocator, ansi, palette, file.language, line.text);
    defer allocator.free(highlighted);

    const color = try lineColor(allocator, ansi, palette, line.kind);
    defer allocator.free(color);
    if (state.mode == .split and width >= 72) {
        const col_width = (width - 15) / 2;
        const left_text = if (line.kind == .delete or line.kind == .context) highlighted else "";
        const right_text = if (line.kind == .add or line.kind == .context) highlighted else "";
        const left = try fitCell(allocator, left_text, col_width);
        defer allocator.free(left);
        const right = try fitCell(allocator, right_text, col_width);
        defer allocator.free(right);
        return std.fmt.allocPrint(allocator, "{s}{s}{s} {s} │ {s}{s} {s}{s}", .{ color, cursor, old_label, left, new_label, if (sign == '+') "+" else " ", right, ansi.reset() });
    }
    return std.fmt.allocPrint(allocator, "{s}{s}{s} {s} {c} {s}{s}", .{ color, cursor, if (selected) "*" else " ", old_label, sign, highlighted, ansi.reset() });
}

fn lineColor(allocator: std.mem.Allocator, ansi: theme.Ansi, palette: theme.ThemeTokens, kind: diff.DiffLineKind) ![]u8 {
    return switch (kind) {
        .add => ansi.fg(allocator, palette.diff_add_fg),
        .delete => ansi.fg(allocator, palette.diff_del_fg),
        .context => ansi.fg(allocator, palette.diff_context_fg),
        .meta => ansi.fg(allocator, palette.fg_muted),
    };
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
    try lines.append(allocator, try util.dupe(allocator, " files"));
    const start = fileTreeStart(snapshot, &state, height);
    const end = @min(snapshot.files.len, start + height - 1);
    for (snapshot.files[start..end], start..) |file, i| {
        const reviewed = store.isReviewed(file.path, file.patch_fingerprint, snapshot.review_target.target_id);
        const comments = store.commentCount(file.path);
        const active = if (i == state.active_file) ">" else " ";
        const mark = if (reviewed) "x" else ".";
        const color = try ansi.fg(allocator, if (reviewed) palette.reviewed_badge else palette.unreviewed_badge);
        defer allocator.free(color);
        const path_width = if (width > 12) width - 12 else width;
        const path = try fitCell(allocator, file.path, path_width);
        defer allocator.free(path);
        const line = try std.fmt.allocPrint(allocator, "{s}{s}{s} {s} {s} ({d}){s}", .{ color, active, file.status.label(), mark, path, comments, ansi.reset() });
        try lines.append(allocator, line);
    }
    return lines.toOwnedSlice(allocator);
}

fn moveLine(snapshot: diff.DiffSnapshot, state: *State, delta: isize) void {
    const total = snapshot.files[state.active_file].lineCount();
    if (total == 0) return;
    state.cursor_line = moveIndex(state.cursor_line, total, delta);
    ensureCursorVisible(snapshot, state);
}

fn scrollLines(snapshot: diff.DiffSnapshot, state: *State, delta: isize) void {
    const total = snapshot.files[state.active_file].lineCount();
    if (total == 0) return;
    state.cursor_line = moveIndex(state.cursor_line, total, delta);
    state.scroll = moveIndex(state.scroll, total, delta);
    ensureCursorVisible(snapshot, state);
}

fn moveFile(snapshot: diff.DiffSnapshot, state: *State, delta: isize) void {
    if (snapshot.files.len == 0) return;
    state.active_file = moveIndex(state.active_file, snapshot.files.len, delta);
    state.cursor_line = 0;
    state.scroll = 0;
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

fn ensureCursorVisible(snapshot: diff.DiffSnapshot, state: *State) void {
    const layout = Layout.init(terminalSize());
    const total = snapshot.files[state.active_file].lineCount();
    if (total == 0) {
        state.scroll = 0;
        return;
    }
    if (state.cursor_line < state.scroll) state.scroll = state.cursor_line;
    if (state.cursor_line >= state.scroll + layout.body_height) {
        state.scroll = state.cursor_line - layout.body_height + 1;
    }
    if (state.scroll >= total) state.scroll = total - 1;
}

fn jumpUnreviewed(snapshot: diff.DiffSnapshot, store: store_mod.Store, state: *State) void {
    for (snapshot.files, 0..) |file, i| {
        if (!store.isReviewed(file.path, file.patch_fingerprint, snapshot.review_target.target_id)) {
            state.active_file = i;
            state.cursor_line = 0;
            state.scroll = 0;
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

fn addCommentAtCursor(
    allocator: std.mem.Allocator,
    snapshot: diff.DiffSnapshot,
    store: *store_mod.Store,
    state: State,
    body: []const u8,
    author: []const u8,
) !void {
    const file = snapshot.files[state.active_file];
    const info = diff.findLineByFlatIndex(file, state.cursor_line) orelse return;
    const line = info.line orelse return;
    var end_line: u32 = 0;
    if (state.selection_start) |start| {
        const last_info = diff.findLineByFlatIndex(file, @max(start, state.cursor_line)) orelse info;
        if (last_info.line) |last| end_line = if (last.kind == .delete) (last.old_lineno orelse 0) else (last.new_lineno orelse last.old_lineno orelse 0);
    }
    var comment = try store.addComment(snapshot.repository.repo_id, snapshot.review_target.target_id, file, line, info.header, end_line, body, author);
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

fn appendCell(allocator: std.mem.Allocator, out: *std.ArrayList(u8), text: []const u8, width: usize) !void {
    const fitted = try fitCell(allocator, text, width);
    defer allocator.free(fitted);
    try out.appendSlice(allocator, fitted);
}

fn fitCell(allocator: std.mem.Allocator, text: []const u8, width: usize) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    var visible: usize = 0;
    var i: usize = 0;
    while (i < text.len and visible < width) {
        if (text[i] == '\x1b') {
            const start = i;
            i += 1;
            while (i < text.len and text[i] != 'm') : (i += 1) {}
            if (i < text.len) i += 1;
            try out.appendSlice(allocator, text[start..i]);
            continue;
        }
        const seq_len = utf8SeqLen(text[i]);
        if (i + seq_len > text.len) break;
        const w = displayWidth(text[i .. i + seq_len]);
        if (visible + w > width) break;
        try out.appendSlice(allocator, text[i .. i + seq_len]);
        visible += w;
        i += seq_len;
    }
    while (visible < width) : (visible += 1) try out.append(allocator, ' ');
    return out.toOwnedSlice(allocator);
}

fn utf8SeqLen(first: u8) usize {
    if (first < 0x80) return 1;
    if ((first & 0xe0) == 0xc0) return 2;
    if ((first & 0xf0) == 0xe0) return 3;
    if ((first & 0xf8) == 0xf0) return 4;
    return 1;
}

fn displayWidth(bytes: []const u8) usize {
    if (bytes.len == 0) return 0;
    if (bytes[0] < 0x20) return 0;
    if (bytes[0] < 0x80) return 1;
    const cp = std.unicode.utf8Decode(bytes) catch return 1;
    if (cp >= 0x1100 and
        (cp <= 0x115f or
            cp == 0x2329 or cp == 0x232a or
            (cp >= 0x2e80 and cp <= 0xa4cf) or
            (cp >= 0xac00 and cp <= 0xd7a3) or
            (cp >= 0xf900 and cp <= 0xfaff) or
            (cp >= 0xfe10 and cp <= 0xfe19) or
            (cp >= 0xfe30 and cp <= 0xfe6f) or
            (cp >= 0xff00 and cp <= 0xff60) or
            (cp >= 0xffe0 and cp <= 0xffe6)))
    {
        return 2;
    }
    return 1;
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

test "display width handles cjk" {
    try std.testing.expectEqual(@as(usize, 1), displayWidth("a"));
    try std.testing.expectEqual(@as(usize, 2), displayWidth("架"));
}

test "file tree start avoids unsigned underflow at first scroll row" {
    try std.testing.expectEqual(@as(usize, 0), fileTreeStartIndex(20, 0, 5));
    try std.testing.expectEqual(@as(usize, 0), fileTreeStartIndex(20, 3, 5));
    try std.testing.expectEqual(@as(usize, 1), fileTreeStartIndex(20, 4, 5));
}
