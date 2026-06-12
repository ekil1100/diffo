const std = @import("std");
const util = @import("util.zig");

pub const ThemeError = error{
    ThemeInvalid,
} || std.mem.Allocator.Error;

pub const Color = struct {
    hex: []const u8,
};

pub const ThemeTokens = struct {
    name: []const u8,
    bg_default: Color,
    bg_panel: Color,
    bg_selected: Color,
    fg_default: Color,
    fg_muted: Color,
    fg_accent: Color,
    diff_add_bg: Color,
    diff_add_fg: Color,
    diff_del_bg: Color,
    diff_del_fg: Color,
    diff_context_fg: Color,
    border: Color,
    warning: Color,
    error_color: Color,
    comment_badge: Color,
    reviewed_badge: Color,
    unreviewed_badge: Color,
    syntax_keyword: Color,
    syntax_string: Color,
    syntax_comment: Color,
    syntax_type: Color,
    syntax_function: Color,
    syntax_number: Color,
    syntax_operator: Color,
    syntax_plain: Color,
};

pub fn catppuccinMocha() ThemeTokens {
    return .{
        .name = "catppuccin-mocha",
        .bg_default = .{ .hex = "#1e1e2e" },
        .bg_panel = .{ .hex = "#181825" },
        .bg_selected = .{ .hex = "#313244" },
        .fg_default = .{ .hex = "#cdd6f4" },
        .fg_muted = .{ .hex = "#9399b2" },
        .fg_accent = .{ .hex = "#89b4fa" },
        .diff_add_bg = .{ .hex = "#1e3a2f" },
        .diff_add_fg = .{ .hex = "#a6e3a1" },
        .diff_del_bg = .{ .hex = "#3b2228" },
        .diff_del_fg = .{ .hex = "#f38ba8" },
        .diff_context_fg = .{ .hex = "#bac2de" },
        .border = .{ .hex = "#45475a" },
        .warning = .{ .hex = "#f9e2af" },
        .error_color = .{ .hex = "#f38ba8" },
        .comment_badge = .{ .hex = "#f5c2e7" },
        .reviewed_badge = .{ .hex = "#a6e3a1" },
        .unreviewed_badge = .{ .hex = "#fab387" },
        .syntax_keyword = .{ .hex = "#cba6f7" },
        .syntax_string = .{ .hex = "#a6e3a1" },
        .syntax_comment = .{ .hex = "#6c7086" },
        .syntax_type = .{ .hex = "#f9e2af" },
        .syntax_function = .{ .hex = "#89b4fa" },
        .syntax_number = .{ .hex = "#fab387" },
        .syntax_operator = .{ .hex = "#94e2d5" },
        .syntax_plain = .{ .hex = "#cdd6f4" },
    };
}

pub const Ansi = struct {
    enabled: bool,
    true_color: bool,

    pub fn init(force: bool) Ansi {
        if (!force and util.envExists("NO_COLOR")) return .{ .enabled = false, .true_color = false };
        const term = (util.envOwned(std.heap.page_allocator, "COLORTERM") catch null) orelse return .{ .enabled = true, .true_color = false };
        defer std.heap.page_allocator.free(term);
        return .{ .enabled = true, .true_color = util.contains(term, "truecolor") or util.contains(term, "24bit") };
    }

    pub fn fg(self: Ansi, allocator: std.mem.Allocator, color: Color) ![]u8 {
        if (!self.enabled) return util.dupe(allocator, "");
        if (!self.true_color) return util.dupe(allocator, "");
        const rgb = parseHex(color.hex) orelse return util.dupe(allocator, "");
        return std.fmt.allocPrint(allocator, "\x1b[38;2;{d};{d};{d}m", .{ rgb.r, rgb.g, rgb.b });
    }

    pub fn bg(self: Ansi, allocator: std.mem.Allocator, color: Color) ![]u8 {
        if (!self.enabled) return util.dupe(allocator, "");
        if (!self.true_color) return util.dupe(allocator, "");
        const rgb = parseHex(color.hex) orelse return util.dupe(allocator, "");
        return std.fmt.allocPrint(allocator, "\x1b[48;2;{d};{d};{d}m", .{ rgb.r, rgb.g, rgb.b });
    }

    pub fn reset(self: Ansi) []const u8 {
        return if (self.enabled) "\x1b[0m" else "";
    }

    /// Write the foreground escape into a caller-owned buffer and return the slice (empty when
    /// colour is disabled). Avoids the per-call heap allocation that fg() incurs on the hot
    /// per-cell / per-span render path. The returned slice borrows `buf`.
    pub fn fgInto(self: Ansi, buf: *[24]u8, color: Color) []const u8 {
        return self.sgrColorInto(buf, "38", color);
    }

    pub fn bgInto(self: Ansi, buf: *[24]u8, color: Color) []const u8 {
        return self.sgrColorInto(buf, "48", color);
    }

    fn sgrColorInto(self: Ansi, buf: *[24]u8, comptime plane: []const u8, color: Color) []const u8 {
        if (!self.enabled or !self.true_color) return "";
        const rgb = parseHex(color.hex) orelse return "";
        return std.fmt.bufPrint(buf, "\x1b[" ++ plane ++ ";2;{d};{d};{d}m", .{ rgb.r, rgb.g, rgb.b }) catch "";
    }

    /// Append the foreground escape directly to `out`, allocation-free.
    pub fn appendFg(self: Ansi, allocator: std.mem.Allocator, out: *std.ArrayList(u8), color: Color) !void {
        var buf: [24]u8 = undefined;
        try out.appendSlice(allocator, self.fgInto(&buf, color));
    }
};

const Rgb = struct { r: u8, g: u8, b: u8 };

fn parseHex(hex: []const u8) ?Rgb {
    const raw = if (hex.len > 0 and hex[0] == '#') hex[1..] else hex;
    if (raw.len != 6) return null;
    return .{
        .r = std.fmt.parseInt(u8, raw[0..2], 16) catch return null,
        .g = std.fmt.parseInt(u8, raw[2..4], 16) catch return null,
        .b = std.fmt.parseInt(u8, raw[4..6], 16) catch return null,
    };
}

pub fn validateBaseThemeFile(allocator: std.mem.Allocator, io: std.Io, path: []const u8) ThemeError![]u8 {
    const content = std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(4 * 1024 * 1024)) catch return error.ThemeInvalid;
    defer allocator.free(content);
    var base16_count: usize = 0;
    var base24_count: usize = 0;
    var idx: u8 = 0;
    while (idx < 24) : (idx += 1) {
        var key_buf: [6]u8 = undefined;
        const key = std.fmt.bufPrint(&key_buf, "base{x:0>2}", .{idx}) catch unreachable;
        // Require each baseNN key to carry a real 6-hex colour on its line, not merely appear as
        // a substring, so arbitrary files that happen to contain "base00" are not accepted.
        if (!keyHasHexColor(content, key)) continue;
        if (idx < 16) base16_count += 1 else base24_count += 1;
    }
    if (base16_count < 16) return error.ThemeInvalid;
    return std.fmt.allocPrint(allocator, "valid Base{s} theme ({d} color slots)", .{ if (base24_count >= 8) "24" else "16", base16_count + base24_count });
}

fn keyHasHexColor(content: []const u8, key: []const u8) bool {
    var search_from: usize = 0;
    while (std.mem.indexOfPos(u8, content, search_from, key)) |key_idx| {
        const after = content[key_idx + key.len ..];
        const line_end = std.mem.indexOfScalar(u8, after, '\n') orelse after.len;
        if (lineContainsHexColor(after[0..line_end])) return true;
        search_from = key_idx + key.len;
    }
    return false;
}

fn lineContainsHexColor(line: []const u8) bool {
    var i: usize = 0;
    while (i + 6 <= line.len) : (i += 1) {
        const before_ok = i == 0 or !std.ascii.isHex(line[i - 1]);
        const after_ok = i + 6 == line.len or !std.ascii.isHex(line[i + 6]);
        if (before_ok and after_ok and parseHex(line[i .. i + 6]) != null) return true;
    }
    return false;
}

pub fn listBuiltins(allocator: std.mem.Allocator) ![]u8 {
    return std.fmt.allocPrint(allocator,
        \\catppuccin-mocha  built-in default
        \\base16/base24     supported via diffo themes validate <file>
        \\
    , .{});
}

test "base16 validator accepts keys" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    var content: std.ArrayList(u8) = .empty;
    defer content.deinit(allocator);
    var idx: u8 = 0;
    while (idx < 16) : (idx += 1) {
        var key_buf: [6]u8 = undefined;
        const key = std.fmt.bufPrint(&key_buf, "base{x:0>2}", .{idx}) catch unreachable;
        try content.appendSlice(allocator, key);
        try content.appendSlice(allocator, ": \"ffffff\"\n");
    }
    try tmp.dir.writeFile(std.testing.io, .{ .sub_path = "theme.yaml", .data = content.items });
    const tmp_path_z = try tmp.dir.realPathFileAlloc(std.testing.io, "theme.yaml", allocator);
    const tmp_path: []const u8 = tmp_path_z;
    defer allocator.free(tmp_path_z);
    const result = try validateBaseThemeFile(allocator, std.testing.io, tmp_path);
    defer allocator.free(result);
    try std.testing.expect(util.contains(result, "Base16"));
}
