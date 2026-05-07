const std = @import("std");
const tree_sitter = @import("tree_sitter.zig");
const util = @import("util.zig");

extern fn tree_sitter_zig() tree_sitter.Language;
extern fn tree_sitter_javascript() tree_sitter.Language;
extern fn tree_sitter_typescript() tree_sitter.Language;
extern fn tree_sitter_tsx() tree_sitter.Language;
extern fn tree_sitter_rust() tree_sitter.Language;
extern fn tree_sitter_c() tree_sitter.Language;
extern fn tree_sitter_cpp() tree_sitter.Language;
extern fn tree_sitter_python() tree_sitter.Language;

pub const Grammar = struct {
    name: []const u8,
    query: []const u8,
    language_fn: *const fn () callconv(.c) tree_sitter.Language,

    pub fn language(self: Grammar) tree_sitter.Language {
        return self.language_fn();
    }
};

const zig_query = @embedFile("syntax_queries/zig_highlights.scm");
const javascript_query =
    @embedFile("syntax_queries/javascript_highlights.scm") ++
    "\n" ++
    @embedFile("syntax_queries/javascript_highlights_params.scm");
const typescript_query = @embedFile("syntax_queries/typescript_highlights.scm");
const tsx_query =
    typescript_query ++
    "\n" ++
    @embedFile("syntax_queries/javascript_highlights_jsx.scm");
const rust_query = @embedFile("syntax_queries/rust_highlights.scm");
const c_query = @embedFile("syntax_queries/c_highlights.scm");
const cpp_query =
    c_query ++
    "\n" ++
    @embedFile("syntax_queries/cpp_highlights.scm");
const python_query = @embedFile("syntax_queries/python_highlights.scm");

pub fn find(language: []const u8) ?Grammar {
    if (util.eql(language, "zig")) {
        return .{
            .name = "zig",
            .query = zig_query,
            .language_fn = tree_sitter_zig,
        };
    }
    if (util.eql(language, "javascript")) {
        return .{
            .name = "javascript",
            .query = javascript_query,
            .language_fn = tree_sitter_javascript,
        };
    }
    if (util.eql(language, "typescript")) {
        return .{
            .name = "typescript",
            .query = typescript_query,
            .language_fn = tree_sitter_typescript,
        };
    }
    if (util.eql(language, "tsx")) {
        return .{
            .name = "tsx",
            .query = tsx_query,
            .language_fn = tree_sitter_tsx,
        };
    }
    if (util.eql(language, "rust")) {
        return .{
            .name = "rust",
            .query = rust_query,
            .language_fn = tree_sitter_rust,
        };
    }
    if (util.eql(language, "c")) {
        return .{
            .name = "c",
            .query = c_query,
            .language_fn = tree_sitter_c,
        };
    }
    if (util.eql(language, "cpp")) {
        return .{
            .name = "cpp",
            .query = cpp_query,
            .language_fn = tree_sitter_cpp,
        };
    }
    if (util.eql(language, "python")) {
        return .{
            .name = "python",
            .query = python_query,
            .language_fn = tree_sitter_python,
        };
    }
    return null;
}

pub fn count() usize {
    return 8;
}

test "finds bundled grammars" {
    const languages = [_][]const u8{ "zig", "javascript", "typescript", "tsx", "rust", "c", "cpp", "python" };
    for (languages) |language| {
        const grammar = find(language) orelse return error.TestExpectedEqual;
        try std.testing.expect(util.eql(grammar.name, language));
        try std.testing.expect(grammar.query.len > 0);
    }
}
