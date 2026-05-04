const std = @import("std");

pub const c = @cImport({
    @cInclude("tree_sitter/api.h");
});

pub const Error = error{
    TreeSitterParserCreateFailed,
    TreeSitterLanguageRejected,
    TreeSitterParseFailed,
    TreeSitterQueryInvalid,
    TreeSitterQueryCursorCreateFailed,
};

pub const Language = *const c.TSLanguage;

pub const Parser = struct {
    raw: *c.TSParser,

    pub fn init(language: Language) Error!Parser {
        const raw = c.ts_parser_new() orelse return error.TreeSitterParserCreateFailed;
        errdefer c.ts_parser_delete(raw);
        if (!c.ts_parser_set_language(raw, language)) return error.TreeSitterLanguageRejected;
        return .{ .raw = raw };
    }

    pub fn deinit(self: *Parser) void {
        c.ts_parser_delete(self.raw);
    }

    pub fn parse(self: Parser, source: []const u8) Error!Tree {
        const raw = c.ts_parser_parse_string(self.raw, null, source.ptr, @intCast(source.len)) orelse return error.TreeSitterParseFailed;
        return .{ .raw = raw };
    }
};

pub const Tree = struct {
    raw: *c.TSTree,

    pub fn deinit(self: *Tree) void {
        c.ts_tree_delete(self.raw);
    }

    pub fn root(self: Tree) c.TSNode {
        return c.ts_tree_root_node(self.raw);
    }
};

pub const Query = struct {
    raw: *c.TSQuery,

    pub fn init(language: Language, source: []const u8) Error!Query {
        var error_offset: u32 = 0;
        var error_type: c.TSQueryError = c.TSQueryErrorNone;
        const raw = c.ts_query_new(language, source.ptr, @intCast(source.len), &error_offset, &error_type) orelse return error.TreeSitterQueryInvalid;
        return .{ .raw = raw };
    }

    pub fn deinit(self: *Query) void {
        c.ts_query_delete(self.raw);
    }
};

pub const QueryCursor = struct {
    raw: *c.TSQueryCursor,

    pub fn init() Error!QueryCursor {
        const raw = c.ts_query_cursor_new() orelse return error.TreeSitterQueryCursorCreateFailed;
        return .{ .raw = raw };
    }

    pub fn deinit(self: *QueryCursor) void {
        c.ts_query_cursor_delete(self.raw);
    }
};

test "tree-sitter parser can parse empty zig source" {
    const grammars = @import("syntax_grammars.zig");
    const grammar = grammars.find("zig") orelse return error.TreeSitterLanguageRejected;
    var parser = try Parser.init(grammar.language());
    defer parser.deinit();
    var tree = try parser.parse("");
    defer tree.deinit();
    _ = tree.root();
}
