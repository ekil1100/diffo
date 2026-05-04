const std = @import("std");
const tree_sitter = @import("tree_sitter.zig");
const util = @import("util.zig");

extern fn tree_sitter_zig() tree_sitter.Language;

pub const Grammar = struct {
    name: []const u8,
    query: []const u8,
    language_fn: *const fn () callconv(.c) tree_sitter.Language,

    pub fn language(self: Grammar) tree_sitter.Language {
        return self.language_fn();
    }
};

const zig_query = @embedFile("syntax_queries/zig_highlights.scm");

pub fn find(language: []const u8) ?Grammar {
    if (util.eql(language, "zig")) {
        return .{
            .name = "zig",
            .query = zig_query,
            .language_fn = tree_sitter_zig,
        };
    }
    return null;
}

pub fn count() usize {
    return 1;
}

test "finds bundled zig grammar" {
    const grammar = find("zig") orelse return error.TestExpectedEqual;
    try std.testing.expect(util.eql(grammar.name, "zig"));
    try std.testing.expect(grammar.query.len > 0);
}
