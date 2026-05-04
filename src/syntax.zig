const std = @import("std");
const syntax_grammars = @import("syntax_grammars.zig");
const theme = @import("theme.zig");
const util = @import("util.zig");

pub const HighlightMode = enum {
    tree_sitter,
    lexical_fallback,
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
        .detail = "Tree-sitter syntax highlighting is available for bundled Zig grammar; unsupported languages and failures use lexical fallback.",
    };
}

pub fn modeForLanguage(language: ?[]const u8) HighlightMode {
    const lang = language orelse return .lexical_fallback;
    return if (syntax_grammars.find(lang) != null) .tree_sitter else .lexical_fallback;
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

pub fn highlightLine(
    allocator: std.mem.Allocator,
    ansi: theme.Ansi,
    tokens: theme.ThemeTokens,
    language: ?[]const u8,
    text: []const u8,
) ![]u8 {
    const lang = language orelse return util.dupe(allocator, text);
    if (util.eql(lang, "markdown") or util.eql(lang, "json") or util.eql(lang, "css") or util.eql(lang, "html")) {
        return highlightSimple(allocator, ansi, tokens, text, &.{});
    }
    const keywords = keywordsFor(lang);
    return highlightSimple(allocator, ansi, tokens, text, keywords);
}

fn highlightSimple(
    allocator: std.mem.Allocator,
    ansi: theme.Ansi,
    tokens: theme.ThemeTokens,
    text: []const u8,
    keywords: []const []const u8,
) ![]u8 {
    if (!ansi.enabled) return util.dupe(allocator, text);
    var out: std.ArrayList(u8) = .empty;
    var i: usize = 0;
    while (i < text.len) {
        const c = text[i];
        if (c == '"' or c == '\'') {
            const color = try ansi.fg(allocator, tokens.syntax_string);
            defer allocator.free(color);
            try out.appendSlice(allocator, color);
            const quote = c;
            try out.append(allocator, c);
            i += 1;
            while (i < text.len) : (i += 1) {
                try out.append(allocator, text[i]);
                if (text[i] == quote and (i == 0 or text[i - 1] != '\\')) {
                    i += 1;
                    break;
                }
            }
            try out.appendSlice(allocator, ansi.reset());
        } else if (std.ascii.isDigit(c)) {
            const color = try ansi.fg(allocator, tokens.syntax_number);
            defer allocator.free(color);
            try out.appendSlice(allocator, color);
            while (i < text.len and (std.ascii.isDigit(text[i]) or text[i] == '.')) : (i += 1) {
                try out.append(allocator, text[i]);
            }
            try out.appendSlice(allocator, ansi.reset());
        } else if (isIdentStart(c)) {
            const start = i;
            i += 1;
            while (i < text.len and isIdent(text[i])) : (i += 1) {}
            const word = text[start..i];
            if (isKeyword(word, keywords)) {
                const color = try ansi.fg(allocator, tokens.syntax_keyword);
                defer allocator.free(color);
                try out.appendSlice(allocator, color);
                try out.appendSlice(allocator, word);
                try out.appendSlice(allocator, ansi.reset());
            } else {
                try out.appendSlice(allocator, word);
            }
        } else {
            try out.append(allocator, c);
            i += 1;
        }
    }
    return out.toOwnedSlice(allocator);
}

fn isIdentStart(c: u8) bool {
    return std.ascii.isAlphabetic(c) or c == '_';
}

fn isIdent(c: u8) bool {
    return std.ascii.isAlphanumeric(c) or c == '_';
}

fn isKeyword(word: []const u8, keywords: []const []const u8) bool {
    for (keywords) |keyword| {
        if (util.eql(word, keyword)) return true;
    }
    return false;
}

fn keywordsFor(language: []const u8) []const []const u8 {
    if (util.eql(language, "zig")) return &.{ "const", "var", "fn", "pub", "return", "try", "catch", "defer", "errdefer", "if", "else", "switch", "for", "while", "struct", "enum", "union", "error", "orelse", "break", "continue" };
    if (util.eql(language, "rust")) return &.{ "fn", "let", "mut", "pub", "impl", "trait", "struct", "enum", "match", "if", "else", "for", "while", "loop", "return", "use", "mod", "crate", "async", "await" };
    if (util.eql(language, "go")) return &.{ "func", "var", "const", "type", "struct", "interface", "return", "if", "else", "for", "range", "switch", "case", "defer", "go", "select", "package", "import" };
    if (util.eql(language, "python")) return &.{ "def", "class", "return", "if", "elif", "else", "for", "while", "try", "except", "finally", "with", "as", "import", "from", "lambda", "yield", "async", "await" };
    if (util.eql(language, "typescript") or util.eql(language, "javascript")) return &.{ "const", "let", "var", "function", "return", "if", "else", "for", "while", "switch", "case", "class", "interface", "type", "import", "export", "async", "await", "new" };
    if (util.eql(language, "c") or util.eql(language, "cpp")) return &.{ "int", "char", "void", "const", "static", "struct", "enum", "return", "if", "else", "for", "while", "switch", "case", "typedef", "class", "namespace" };
    return &.{};
}

test "registry has graceful fallback" {
    try std.testing.expectEqual(HighlightMode.tree_sitter, registryStatus().mode);
}

test "language mode reports fallback for unsupported languages" {
    try std.testing.expectEqual(HighlightMode.tree_sitter, modeForLanguage("zig"));
    try std.testing.expectEqual(HighlightMode.lexical_fallback, modeForLanguage("markdown"));
    try std.testing.expectEqual(HighlightMode.lexical_fallback, modeForLanguage(null));
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
