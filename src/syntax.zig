const std = @import("std");
const syntax_grammars = @import("syntax_grammars.zig");
const theme = @import("theme.zig");
const util = @import("util.zig");

pub const HighlightMode = enum {
    tree_sitter,
    disabled,
};

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

pub const HighlightSpan = struct {
    start_byte: usize,
    end_byte: usize,
    token: SyntaxToken,
};

pub const Config = struct {
    enabled: bool = true,
    tree_sitter_ast_enabled: bool = true,
    max_file_size: usize = 512 * 1024,
};

pub const RegistryStatus = struct {
    mode: HighlightMode,
    detail: []const u8,
};

pub fn registryStatus() RegistryStatus {
    return .{
        .mode = .tree_sitter,
        .detail = "Tree-sitter syntax highlighting is available for bundled Zig, TS, TSX, JS, Rust, C, C++, and Python grammars.",
    };
}

pub fn modeForLanguage(language: ?[]const u8) HighlightMode {
    const lang = language orelse return .disabled;
    return if (syntax_grammars.find(lang) != null) .tree_sitter else .disabled;
}

pub fn renderHighlightedLine(
    allocator: std.mem.Allocator,
    ansi: theme.Ansi,
    tokens: theme.ThemeTokens,
    text: []const u8,
    spans: []const HighlightSpan,
) ![]u8 {
    if (!ansi.enabled or spans.len == 0) return util.dupe(allocator, text);
    var out: std.ArrayList(u8) = .empty;
    var cursor: usize = 0;
    for (spans) |span| {
        if (span.end_byte <= cursor or span.start_byte >= text.len or span.end_byte > text.len or span.start_byte >= span.end_byte) continue;
        const start = @max(cursor, span.start_byte);
        if (cursor < start) try out.appendSlice(allocator, text[cursor..start]);
        const color = try ansi.fg(allocator, colorForToken(tokens, span.token));
        defer allocator.free(color);
        try out.appendSlice(allocator, color);
        try out.appendSlice(allocator, text[start..span.end_byte]);
        try out.appendSlice(allocator, ansi.reset());
        cursor = span.end_byte;
    }
    if (cursor < text.len) try out.appendSlice(allocator, text[cursor..]);
    return out.toOwnedSlice(allocator);
}

fn colorForToken(tokens: theme.ThemeTokens, token: SyntaxToken) theme.Color {
    return switch (token) {
        .keyword => tokens.syntax_keyword,
        .string => tokens.syntax_string,
        .comment => tokens.syntax_comment,
        .type => tokens.syntax_type,
        .function => tokens.syntax_function,
        .number => tokens.syntax_number,
        .operator => tokens.syntax_operator,
        .plain => tokens.syntax_plain,
    };
}

test "registry reports bundled tree-sitter grammars" {
    try std.testing.expectEqual(HighlightMode.tree_sitter, registryStatus().mode);
}

test "language mode reports disabled for unsupported languages" {
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("zig"));
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("typescript"));
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("tsx"));
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("javascript"));
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("rust"));
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("c"));
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("cpp"));
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("python"));
    try std.testing.expectEqual(HighlightMode.disabled, modeForLanguage("markdown"));
    try std.testing.expectEqual(HighlightMode.disabled, modeForLanguage(null));
}

test "render highlighted line emits token color" {
    const allocator = std.testing.allocator;
    const ansi = theme.Ansi{ .enabled = true, .true_color = true };
    const rendered = try renderHighlightedLine(allocator, ansi, theme.catppuccinMocha(), "const value = 1", &.{
        .{ .start_byte = 0, .end_byte = 5, .token = .keyword },
    });
    defer allocator.free(rendered);
    try std.testing.expect(util.contains(rendered, "\x1b[38;2;"));
    try std.testing.expect(util.contains(rendered, "const"));
}
