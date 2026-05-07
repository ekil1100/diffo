const std = @import("std");
const syntax = @import("syntax.zig");
const tree_sitter = @import("tree_sitter.zig");
const util = @import("util.zig");

const c = tree_sitter.c;

pub const SideHighlight = struct {
    source: []u8,
    line_starts: []usize,
    spans_by_line: [][]syntax.HighlightSpan,

    pub fn deinit(self: *SideHighlight, allocator: std.mem.Allocator) void {
        for (self.spans_by_line) |spans| allocator.free(spans);
        allocator.free(self.spans_by_line);
        allocator.free(self.line_starts);
        allocator.free(self.source);
    }

    pub fn lineText(self: SideHighlight, line_number: u32) ?[]const u8 {
        if (line_number == 0) return null;
        const index: usize = @intCast(line_number - 1);
        if (index >= self.line_starts.len) return null;
        const start = self.line_starts[index];
        const end = lineEnd(self.source, self.line_starts, index);
        return util.trimLine(self.source[start..end]);
    }

    pub fn lineSpans(self: SideHighlight, line_number: u32) []const syntax.HighlightSpan {
        if (line_number == 0) return &.{};
        const index: usize = @intCast(line_number - 1);
        if (index >= self.spans_by_line.len) return &.{};
        return self.spans_by_line[index];
    }
};

pub fn build(
    allocator: std.mem.Allocator,
    language: tree_sitter.Language,
    query_source: []const u8,
    source: []u8,
) !SideHighlight {
    errdefer allocator.free(source);
    const line_starts = try buildLineStarts(allocator, source);
    errdefer allocator.free(line_starts);

    const line_spans = try allocator.alloc(std.ArrayList(syntax.HighlightSpan), line_starts.len);
    defer allocator.free(line_spans);
    for (line_spans) |*line| line.* = .empty;
    errdefer {
        for (line_spans) |*line| line.deinit(allocator);
    }

    var parser = try tree_sitter.Parser.init(language);
    defer parser.deinit();
    var tree = try parser.parse(source);
    defer tree.deinit();
    var query = try tree_sitter.Query.init(language, query_source);
    defer query.deinit();
    var cursor = try tree_sitter.QueryCursor.init();
    defer cursor.deinit();

    c.ts_query_cursor_exec(cursor.raw, query.raw, tree.root());
    var match: c.TSQueryMatch = undefined;
    while (c.ts_query_cursor_next_match(cursor.raw, &match)) {
        if (!predicatesPass(query.raw, match, source)) continue;
        const captures = match.captures[0..match.capture_count];
        for (captures) |capture| {
            const name = captureName(query.raw, capture.index);
            const token = tokenForCapture(name) orelse continue;
            appendCaptureSpan(allocator, line_spans, line_starts, source, capture.node, token) catch |err| return err;
        }
    }

    var spans_by_line = try allocator.alloc([]syntax.HighlightSpan, line_spans.len);
    errdefer allocator.free(spans_by_line);
    for (line_spans, 0..) |*spans, i| {
        std.mem.sort(syntax.HighlightSpan, spans.items, {}, lessSpan);
        spans_by_line[i] = try spans.toOwnedSlice(allocator);
    }

    return .{
        .source = source,
        .line_starts = line_starts,
        .spans_by_line = spans_by_line,
    };
}

fn buildLineStarts(allocator: std.mem.Allocator, source: []const u8) ![]usize {
    var starts: std.ArrayList(usize) = .empty;
    try starts.append(allocator, 0);
    for (source, 0..) |byte, i| {
        if (byte == '\n' and i + 1 < source.len) try starts.append(allocator, i + 1);
    }
    return starts.toOwnedSlice(allocator);
}

fn lineEnd(source: []const u8, line_starts: []const usize, index: usize) usize {
    if (index + 1 < line_starts.len) {
        const next = line_starts[index + 1];
        if (next > 0 and source[next - 1] == '\n') return next - 1;
        return next;
    }
    return source.len;
}

fn appendCaptureSpan(
    allocator: std.mem.Allocator,
    line_spans: []std.ArrayList(syntax.HighlightSpan),
    line_starts: []const usize,
    source: []const u8,
    node: c.TSNode,
    token: syntax.SyntaxToken,
) !void {
    const start_point = c.ts_node_start_point(node);
    const end_point = c.ts_node_end_point(node);
    const start_byte: usize = @intCast(c.ts_node_start_byte(node));
    const end_byte: usize = @intCast(c.ts_node_end_byte(node));
    if (end_byte <= start_byte or start_point.row >= line_spans.len) return;
    const last_row: usize = @min(@as(usize, @intCast(end_point.row)), line_spans.len - 1);
    var row: usize = @intCast(start_point.row);
    while (row <= last_row) : (row += 1) {
        const absolute_start = if (row == start_point.row) start_byte else line_starts[row];
        const absolute_end = if (row == end_point.row) end_byte else lineEnd(source, line_starts, row);
        if (absolute_end <= absolute_start) continue;
        try line_spans[row].append(allocator, .{
            .start_byte = absolute_start - line_starts[row],
            .end_byte = absolute_end - line_starts[row],
            .token = token,
        });
    }
}

fn lessSpan(_: void, a: syntax.HighlightSpan, b: syntax.HighlightSpan) bool {
    if (a.start_byte == b.start_byte) return a.end_byte < b.end_byte;
    return a.start_byte < b.start_byte;
}

fn tokenForCapture(name: []const u8) ?syntax.SyntaxToken {
    if (std.mem.startsWith(u8, name, "keyword")) return .keyword;
    if (std.mem.startsWith(u8, name, "string") or util.eql(name, "character")) return .string;
    if (std.mem.startsWith(u8, name, "comment")) return .comment;
    if (std.mem.startsWith(u8, name, "type") or util.eql(name, "constructor")) return .type;
    if (std.mem.startsWith(u8, name, "function") or std.mem.startsWith(u8, name, "method")) return .function;
    if (std.mem.startsWith(u8, name, "number") or std.mem.startsWith(u8, name, "constant.numeric") or util.eql(name, "float") or util.eql(name, "integer")) return .number;
    if (std.mem.startsWith(u8, name, "constant") or util.eql(name, "boolean") or util.eql(name, "null")) return .keyword;
    if (std.mem.startsWith(u8, name, "operator") or std.mem.startsWith(u8, name, "punctuation")) return .operator;
    if (std.mem.startsWith(u8, name, "property") or std.mem.startsWith(u8, name, "attribute") or util.eql(name, "tag")) return .function;
    if (std.mem.startsWith(u8, name, "escape")) return .string;
    return null;
}

fn predicatesPass(query: *c.TSQuery, match: c.TSQueryMatch, source: []const u8) bool {
    var step_count: u32 = 0;
    const raw_steps = c.ts_query_predicates_for_pattern(query, match.pattern_index, &step_count);
    if (step_count == 0) return true;
    const steps = raw_steps[0..step_count];
    var start: usize = 0;
    var i: usize = 0;
    while (i < steps.len) : (i += 1) {
        if (steps[i].type == c.TSQueryPredicateStepTypeDone) {
            if (!predicatePass(query, match, source, steps[start..i])) return false;
            start = i + 1;
        }
    }
    return true;
}

fn predicatePass(query: *c.TSQuery, match: c.TSQueryMatch, source: []const u8, steps: []const c.TSQueryPredicateStep) bool {
    if (steps.len == 0 or steps[0].type != c.TSQueryPredicateStepTypeString) return false;
    const op = queryString(query, steps[0].value_id);
    const normalized = if (std.mem.startsWith(u8, op, "#")) op[1..] else op;
    if (util.eql(normalized, "set!") or util.eql(normalized, "is?") or util.eql(normalized, "is-not?")) return true;
    if (util.eql(normalized, "eq?") or util.eql(normalized, "not-eq?")) {
        if (steps.len != 3) return false;
        const left = predicateValue(query, match, source, steps[1]) orelse return false;
        const right = predicateValue(query, match, source, steps[2]) orelse return false;
        const same = util.eql(left, right);
        return if (util.eql(normalized, "eq?")) same else !same;
    }
    if (util.eql(normalized, "any-of?")) {
        if (steps.len < 3 or steps[1].type != c.TSQueryPredicateStepTypeCapture) return false;
        const value = captureText(match, source, steps[1].value_id) orelse return false;
        for (steps[2..]) |step| {
            if (step.type != c.TSQueryPredicateStepTypeString) return false;
            if (util.eql(value, queryString(query, step.value_id))) return true;
        }
        return false;
    }
    if (util.eql(normalized, "match?") or util.eql(normalized, "not-match?") or util.eql(normalized, "lua-match?")) {
        if (steps.len != 3 or steps[1].type != c.TSQueryPredicateStepTypeCapture or steps[2].type != c.TSQueryPredicateStepTypeString) return false;
        const value = captureText(match, source, steps[1].value_id) orelse return false;
        const pattern = queryString(query, steps[2].value_id);
        const matched = matchSupportedPattern(value, pattern);
        if (util.eql(normalized, "not-match?")) return !matched;
        return matched;
    }
    return false;
}

fn predicateValue(query: *c.TSQuery, match: c.TSQueryMatch, source: []const u8, step: c.TSQueryPredicateStep) ?[]const u8 {
    return switch (step.type) {
        c.TSQueryPredicateStepTypeCapture => captureText(match, source, step.value_id),
        c.TSQueryPredicateStepTypeString => queryString(query, step.value_id),
        else => null,
    };
}

fn captureText(match: c.TSQueryMatch, source: []const u8, capture_id: u32) ?[]const u8 {
    const captures = match.captures[0..match.capture_count];
    for (captures) |capture| {
        if (capture.index != capture_id) continue;
        const start: usize = @intCast(c.ts_node_start_byte(capture.node));
        const end: usize = @intCast(c.ts_node_end_byte(capture.node));
        if (end < start or end > source.len) return null;
        return source[start..end];
    }
    return null;
}

fn captureName(query: *c.TSQuery, index: u32) []const u8 {
    var len: u32 = 0;
    const ptr = c.ts_query_capture_name_for_id(query, index, &len);
    return ptr[0..len];
}

fn queryString(query: *c.TSQuery, index: u32) []const u8 {
    var len: u32 = 0;
    const ptr = c.ts_query_string_value_for_id(query, index, &len);
    return ptr[0..len];
}

fn matchSupportedPattern(value: []const u8, pattern: []const u8) bool {
    if (util.eql(pattern, "^[A-Z]")) {
        return value.len > 0 and std.ascii.isUpper(value[0]);
    }
    if (util.eql(pattern, "^[A-Z_][a-zA-Z0-9_]*")) {
        if (value.len == 0) return false;
        if (!(std.ascii.isUpper(value[0]) or value[0] == '_')) return false;
        for (value[1..]) |byte| {
            if (!(std.ascii.isAlphanumeric(byte) or byte == '_')) return false;
        }
        return true;
    }
    if (util.eql(pattern, "^[A-Z][A-Z_0-9]+$")) {
        if (value.len < 2 or !std.ascii.isUpper(value[0])) return false;
        for (value[1..]) |byte| {
            if (!(std.ascii.isUpper(byte) or std.ascii.isDigit(byte) or byte == '_')) return false;
        }
        return true;
    }
    if (util.eql(pattern, "^[A-Z][A-Z\\d_]+$") or util.eql(pattern, "^[A-Z][A-Z\\d_]*$") or util.eql(pattern, "^[A-Z][A-Z_]*$")) {
        if (value.len == 0 or !std.ascii.isUpper(value[0])) return false;
        for (value[1..]) |byte| {
            if (!(std.ascii.isUpper(byte) or std.ascii.isDigit(byte) or byte == '_')) return false;
        }
        return true;
    }
    if (util.eql(pattern, "^[a-z][^.]*$")) {
        if (value.len == 0 or !std.ascii.isLower(value[0])) return false;
        return std.mem.indexOfScalar(u8, value, '.') == null;
    }
    if (std.mem.startsWith(u8, pattern, "^(") and std.mem.endsWith(u8, pattern, ")$")) {
        var alternatives = std.mem.splitScalar(u8, pattern[2 .. pattern.len - 2], '|');
        while (alternatives.next()) |alternative| {
            if (util.eql(value, alternative)) return true;
        }
        return false;
    }
    if (std.mem.startsWith(u8, pattern, "^")) {
        return std.mem.startsWith(u8, value, pattern[1..]);
    }
    return false;
}

test "capture mapping handles common captures" {
    try std.testing.expectEqual(syntax.SyntaxToken.keyword, tokenForCapture("keyword.return").?);
    try std.testing.expectEqual(syntax.SyntaxToken.function, tokenForCapture("function.call").?);
    try std.testing.expectEqual(syntax.SyntaxToken.comment, tokenForCapture("comment.documentation").?);
}

test "supported lua-match predicates are conservative" {
    try std.testing.expect(matchSupportedPattern("MyType", "^[A-Z_][a-zA-Z0-9_]*"));
    try std.testing.expect(!matchSupportedPattern("myType", "^[A-Z_][a-zA-Z0-9_]*"));
    try std.testing.expect(matchSupportedPattern("//! doc", "^//!"));
    try std.testing.expect(matchSupportedPattern("MyType", "^[A-Z]"));
    try std.testing.expect(matchSupportedPattern("MAX_VALUE", "^[A-Z][A-Z\\d_]+$"));
    try std.testing.expect(matchSupportedPattern("console", "^(arguments|module|console|window|document)$"));
}

test "zig query produces keyword spans" {
    const allocator = std.testing.allocator;
    const grammars = @import("syntax_grammars.zig");
    const grammar = grammars.find("zig") orelse return error.TestUnexpectedResult;
    const source = try allocator.dupe(u8, "const value = 1;\n");
    var highlight = try build(allocator, grammar.language(), grammar.query, source);
    defer highlight.deinit(allocator);
    const spans = highlight.lineSpans(1);
    try std.testing.expect(spans.len > 0);
    try std.testing.expectEqual(syntax.SyntaxToken.keyword, spans[0].token);
}

test "bundled tree-sitter grammars produce highlight spans" {
    const allocator = std.testing.allocator;
    const grammars = @import("syntax_grammars.zig");
    const samples = [_]struct {
        language: []const u8,
        source: []const u8,
    }{
        .{ .language = "javascript", .source = "const value = 1;\n" },
        .{ .language = "typescript", .source = "const value: number = 1;\n" },
        .{ .language = "tsx", .source = "const view = <div>{value}</div>;\n" },
        .{ .language = "rust", .source = "fn main() { let value: usize = 1; }\n" },
        .{ .language = "c", .source = "int main(void) { return 0; }\n" },
        .{ .language = "cpp", .source = "class App { public: int run(); };\n" },
        .{ .language = "python", .source = "def run() -> bool:\n    return True\n" },
    };
    for (samples) |sample| {
        const grammar = grammars.find(sample.language) orelse return error.TestUnexpectedResult;
        const source = try allocator.dupe(u8, sample.source);
        var highlight = build(allocator, grammar.language(), grammar.query, source) catch |err| {
            std.debug.print("failed to build syntax query for {s}: {s}\n", .{ sample.language, @errorName(err) });
            return err;
        };
        defer highlight.deinit(allocator);
        try std.testing.expect(highlight.lineSpans(1).len > 0);
    }
}

test "typescript tree-sitter query highlights ordinary keywords" {
    const allocator = std.testing.allocator;
    const grammars = @import("syntax_grammars.zig");
    const grammar = grammars.find("typescript") orelse return error.TestUnexpectedResult;
    const source = try allocator.dupe(u8, "const value: number = 1;\nfunction run() { return value; }\n");
    var highlight = try build(allocator, grammar.language(), grammar.query, source);
    defer highlight.deinit(allocator);

    try std.testing.expect(hasTokenText(
        highlight.lineText(1).?,
        highlight.lineSpans(1),
        .keyword,
        "const",
    ));
    try std.testing.expect(hasTokenText(
        highlight.lineText(2).?,
        highlight.lineSpans(2),
        .keyword,
        "function",
    ));
    try std.testing.expect(hasTokenText(
        highlight.lineText(2).?,
        highlight.lineSpans(2),
        .keyword,
        "return",
    ));
}

fn hasTokenText(line: []const u8, spans: []const syntax.HighlightSpan, token: syntax.SyntaxToken, text: []const u8) bool {
    for (spans) |span| {
        if (span.token != token or span.end_byte > line.len) continue;
        if (util.eql(line[span.start_byte..span.end_byte], text)) return true;
    }
    return false;
}
