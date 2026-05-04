const std = @import("std");
const tui_text = @import("tui_text.zig");

pub const Range = struct {
    start: usize,
    end: usize,
};

pub const PairRanges = struct {
    old: []Range,
    new: []Range,

    pub fn deinit(self: *PairRanges, allocator: std.mem.Allocator) void {
        allocator.free(self.old);
        allocator.free(self.new);
    }
};

const TokenKind = enum {
    whitespace,
    word,
    punct,
};

const Token = struct {
    start: usize,
    end: usize,
    kind: TokenKind,
};

const max_lcs_tokens: usize = 240;
const max_lcs_cells: usize = 24_000;

pub fn diffRanges(allocator: std.mem.Allocator, old_text: []const u8, new_text: []const u8) !PairRanges {
    if (std.mem.eql(u8, old_text, new_text)) {
        return .{
            .old = try allocator.alloc(Range, 0),
            .new = try allocator.alloc(Range, 0),
        };
    }

    const old_tokens = try tokenize(allocator, old_text);
    defer allocator.free(old_tokens);
    const new_tokens = try tokenize(allocator, new_text);
    defer allocator.free(new_tokens);

    if (old_tokens.len == 0 or new_tokens.len == 0 or
        old_tokens.len > max_lcs_tokens or
        new_tokens.len > max_lcs_tokens or
        old_tokens.len * new_tokens.len > max_lcs_cells)
    {
        return affixRanges(allocator, old_text, new_text);
    }

    const old_matched = try allocator.alloc(bool, old_tokens.len);
    defer allocator.free(old_matched);
    @memset(old_matched, false);
    const new_matched = try allocator.alloc(bool, new_tokens.len);
    defer allocator.free(new_matched);
    @memset(new_matched, false);

    try markLcs(allocator, old_text, old_tokens, new_text, new_tokens, old_matched, new_matched);

    const old_ranges = try rangesFromTokens(allocator, old_tokens, old_matched);
    errdefer allocator.free(old_ranges);
    const new_ranges = try rangesFromTokens(allocator, new_tokens, new_matched);

    return .{
        .old = old_ranges,
        .new = new_ranges,
    };
}

fn tokenize(allocator: std.mem.Allocator, text: []const u8) ![]Token {
    var tokens: std.ArrayList(Token) = .empty;
    errdefer tokens.deinit(allocator);

    var i: usize = 0;
    while (i < text.len) {
        const start = i;
        if (isWhitespace(text[i])) {
            while (i < text.len and isWhitespace(text[i])) : (i += 1) {}
            try tokens.append(allocator, .{ .start = start, .end = i, .kind = .whitespace });
            continue;
        }
        if (isWordByte(text[i])) {
            while (i < text.len and isWordByte(text[i])) : (i += 1) {}
            try tokens.append(allocator, .{ .start = start, .end = i, .kind = .word });
            continue;
        }

        const seq_len = tui_text.utf8SeqLen(text[i]);
        i += if (i + seq_len <= text.len) seq_len else 1;
        try tokens.append(allocator, .{ .start = start, .end = i, .kind = .punct });
    }

    return tokens.toOwnedSlice(allocator);
}

fn markLcs(
    allocator: std.mem.Allocator,
    old_text: []const u8,
    old_tokens: []const Token,
    new_text: []const u8,
    new_tokens: []const Token,
    old_matched: []bool,
    new_matched: []bool,
) !void {
    const cols = new_tokens.len + 1;
    var dp = try allocator.alloc(usize, (old_tokens.len + 1) * cols);
    defer allocator.free(dp);
    @memset(dp, 0);

    var i = old_tokens.len;
    while (i > 0) {
        i -= 1;
        var j = new_tokens.len;
        while (j > 0) {
            j -= 1;
            const here = i * cols + j;
            if (tokenEqual(old_text, old_tokens[i], new_text, new_tokens[j])) {
                dp[here] = dp[(i + 1) * cols + (j + 1)] + 1;
            } else {
                dp[here] = @max(dp[(i + 1) * cols + j], dp[i * cols + (j + 1)]);
            }
        }
    }

    i = 0;
    var j: usize = 0;
    while (i < old_tokens.len and j < new_tokens.len) {
        if (tokenEqual(old_text, old_tokens[i], new_text, new_tokens[j])) {
            old_matched[i] = true;
            new_matched[j] = true;
            i += 1;
            j += 1;
        } else if (dp[(i + 1) * cols + j] >= dp[i * cols + (j + 1)]) {
            i += 1;
        } else {
            j += 1;
        }
    }
}

fn rangesFromTokens(allocator: std.mem.Allocator, tokens: []const Token, matched: []const bool) ![]Range {
    var ranges: std.ArrayList(Range) = .empty;
    errdefer ranges.deinit(allocator);

    var i: usize = 0;
    while (i < tokens.len) {
        if (matched[i]) {
            i += 1;
            continue;
        }

        const run_start = i;
        var has_significant = false;
        while (i < tokens.len and !matched[i]) : (i += 1) {
            if (tokens[i].kind != .whitespace) has_significant = true;
        }
        var start = run_start;
        var end = i;
        if (has_significant) {
            while (start < end and tokens[start].kind == .whitespace) : (start += 1) {}
            while (end > start and tokens[end - 1].kind == .whitespace) : (end -= 1) {}
        }
        if (start < end) try appendRange(allocator, &ranges, .{
            .start = tokens[start].start,
            .end = tokens[end - 1].end,
        });
    }

    return ranges.toOwnedSlice(allocator);
}

fn affixRanges(allocator: std.mem.Allocator, old_text: []const u8, new_text: []const u8) !PairRanges {
    const prefix = commonPrefix(old_text, new_text);
    const old_suffix = commonSuffix(old_text[prefix..], new_text[prefix..]);
    const new_suffix = old_suffix;

    var old_ranges: std.ArrayList(Range) = .empty;
    errdefer old_ranges.deinit(allocator);
    var new_ranges: std.ArrayList(Range) = .empty;
    errdefer new_ranges.deinit(allocator);

    if (prefix < old_text.len - old_suffix) try old_ranges.append(allocator, .{ .start = prefix, .end = old_text.len - old_suffix });
    if (prefix < new_text.len - new_suffix) try new_ranges.append(allocator, .{ .start = prefix, .end = new_text.len - new_suffix });

    const old_owned = try old_ranges.toOwnedSlice(allocator);
    errdefer allocator.free(old_owned);
    const new_owned = try new_ranges.toOwnedSlice(allocator);

    return .{
        .old = old_owned,
        .new = new_owned,
    };
}

fn appendRange(allocator: std.mem.Allocator, ranges: *std.ArrayList(Range), range: Range) !void {
    if (range.end <= range.start) return;
    if (ranges.items.len > 0 and ranges.items[ranges.items.len - 1].end >= range.start) {
        ranges.items[ranges.items.len - 1].end = @max(ranges.items[ranges.items.len - 1].end, range.end);
        return;
    }
    try ranges.append(allocator, range);
}

fn tokenEqual(left_text: []const u8, left: Token, right_text: []const u8, right: Token) bool {
    return std.mem.eql(u8, left_text[left.start..left.end], right_text[right.start..right.end]);
}

fn commonPrefix(left: []const u8, right: []const u8) usize {
    var i: usize = 0;
    var last_boundary: usize = 0;
    while (i < left.len and i < right.len and left[i] == right[i]) {
        i += 1;
        if (isUtf8Boundary(left, i) and isUtf8Boundary(right, i)) last_boundary = i;
    }
    return last_boundary;
}

fn commonSuffix(left: []const u8, right: []const u8) usize {
    var count: usize = 0;
    while (count < left.len and count < right.len and left[left.len - count - 1] == right[right.len - count - 1]) {
        count += 1;
    }
    while (count > 0 and (!isUtf8Boundary(left, left.len - count) or !isUtf8Boundary(right, right.len - count))) {
        count -= 1;
    }
    return count;
}

fn isUtf8Boundary(text: []const u8, index: usize) bool {
    return index == 0 or index == text.len or (text[index] & 0xc0) != 0x80;
}

fn isWhitespace(byte: u8) bool {
    return byte == ' ' or byte == '\t' or byte == '\r' or byte == '\n';
}

fn isWordByte(byte: u8) bool {
    return std.ascii.isAlphanumeric(byte) or byte == '_';
}

test "token diff keeps moved common run unhighlighted" {
    const allocator = std.testing.allocator;
    var ranges = try diffRanges(
        allocator,
        "    snapshot.files[state.active_file], store, view, state,",
        "    state, snapshot.files[state.active_file],",
    );
    defer ranges.deinit(allocator);

    try std.testing.expectEqual(@as(usize, 1), ranges.old.len);
    try std.testing.expectEqualStrings("store, view, state,", "    snapshot.files[state.active_file], store, view, state,"[ranges.old[0].start..ranges.old[0].end]);
    try std.testing.expectEqual(@as(usize, 1), ranges.new.len);
    try std.testing.expectEqualStrings("state,", "    state, snapshot.files[state.active_file],"[ranges.new[0].start..ranges.new[0].end]);
}

test "token diff marks only changed suffix" {
    const allocator = std.testing.allocator;
    var ranges = try diffRanges(allocator, "const name = oldValue;", "const name = newValue;");
    defer ranges.deinit(allocator);

    try std.testing.expectEqual(@as(usize, 1), ranges.old.len);
    try std.testing.expectEqualStrings("oldValue", "const name = oldValue;"[ranges.old[0].start..ranges.old[0].end]);
    try std.testing.expectEqual(@as(usize, 1), ranges.new.len);
    try std.testing.expectEqualStrings("newValue", "const name = newValue;"[ranges.new[0].start..ranges.new[0].end]);
}

test "token diff preserves whitespace-only changes" {
    const allocator = std.testing.allocator;
    var ranges = try diffRanges(allocator, "call(a, b)", "call(a,b)");
    defer ranges.deinit(allocator);

    try std.testing.expectEqual(@as(usize, 1), ranges.old.len);
    try std.testing.expectEqualStrings(" ", "call(a, b)"[ranges.old[0].start..ranges.old[0].end]);
    try std.testing.expectEqual(@as(usize, 0), ranges.new.len);
}
