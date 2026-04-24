const std = @import("std");
const theme = @import("theme.zig");
const util = @import("util.zig");

pub const HighlightMode = enum {
    tree_sitter,
    lexical_fallback,
    disabled,
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
        .mode = .lexical_fallback,
        .detail = "Tree-sitter adapter is present as a boundary, but no grammars are bundled; lexical syntax highlighting is used and diff semantic highlighting remains authoritative.",
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
    try std.testing.expectEqual(HighlightMode.lexical_fallback, registryStatus().mode);
}
