const std = @import("std");

pub fn dupe(allocator: std.mem.Allocator, value: []const u8) ![]u8 {
    return allocator.dupe(u8, value);
}

pub fn trimLine(line: []const u8) []const u8 {
    return std.mem.trimEnd(u8, line, "\r");
}

pub fn startsWith(haystack: []const u8, needle: []const u8) bool {
    return std.mem.startsWith(u8, haystack, needle);
}

pub fn contains(haystack: []const u8, needle: []const u8) bool {
    return std.mem.indexOf(u8, haystack, needle) != null;
}

pub fn hashHex(allocator: std.mem.Allocator, bytes: []const u8) ![]u8 {
    var digest: [32]u8 = undefined;
    std.crypto.hash.sha2.Sha256.hash(bytes, &digest, .{});
    return bytesToHex(allocator, &digest);
}

pub fn bytesToHex(allocator: std.mem.Allocator, bytes: []const u8) ![]u8 {
    const alphabet = "0123456789abcdef";
    var out = try allocator.alloc(u8, bytes.len * 2);
    for (bytes, 0..) |b, i| {
        out[i * 2] = alphabet[b >> 4];
        out[i * 2 + 1] = alphabet[b & 0x0f];
    }
    return out;
}

pub fn nowIso(allocator: std.mem.Allocator, io: std.Io) ![]u8 {
    const now = std.Io.Clock.real.now(io);
    const ts_i = @divTrunc(now.nanoseconds, std.time.ns_per_s);
    const ts: u64 = if (ts_i < 0) 0 else @intCast(ts_i);
    const epoch_seconds = std.time.epoch.EpochSeconds{ .secs = ts };
    const year_day = epoch_seconds.getEpochDay().calculateYearDay();
    const month_day = year_day.calculateMonthDay();
    const day_seconds = epoch_seconds.getDaySeconds();
    return std.fmt.allocPrint(
        allocator,
        "{d:0>4}-{d:0>2}-{d:0>2}T{d:0>2}:{d:0>2}:{d:0>2}Z",
        .{
            year_day.year,
            month_day.month.numeric(),
            month_day.day_index + 1,
            day_seconds.getHoursIntoDay(),
            day_seconds.getMinutesIntoHour(),
            day_seconds.getSecondsIntoMinute(),
        },
    );
}

pub fn joinArgs(allocator: std.mem.Allocator, args: []const []const u8) ![]u8 {
    if (args.len == 0) return dupe(allocator, "working-tree");
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);
    for (args, 0..) |arg, i| {
        if (i > 0) try out.append(allocator, ' ');
        try out.appendSlice(allocator, arg);
    }
    return out.toOwnedSlice(allocator);
}

fn jsonEscape(allocator: std.mem.Allocator, value: []const u8) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);
    var i: usize = 0;
    while (i < value.len) {
        const c = value[i];
        switch (c) {
            '"' => {
                try out.appendSlice(allocator, "\\\"");
                i += 1;
            },
            '\\' => {
                try out.appendSlice(allocator, "\\\\");
                i += 1;
            },
            '\n' => {
                try out.appendSlice(allocator, "\\n");
                i += 1;
            },
            '\r' => {
                try out.appendSlice(allocator, "\\r");
                i += 1;
            },
            '\t' => {
                try out.appendSlice(allocator, "\\t");
                i += 1;
            },
            else => {
                if (c < 0x20) {
                    var buf: [8]u8 = undefined;
                    const escaped = std.fmt.bufPrint(&buf, "\\u{x:0>4}", .{c}) catch unreachable;
                    try out.appendSlice(allocator, escaped);
                    i += 1;
                } else if (c < 0x80) {
                    try out.append(allocator, c);
                    i += 1;
                } else {
                    // Multi-byte UTF-8: emit valid sequences verbatim (valid JSON),
                    // and replace any invalid byte with U+FFFD so the result is always
                    // valid UTF-8 / parseable JSON regardless of argv or git path bytes.
                    const seq_len = std.unicode.utf8ByteSequenceLength(c) catch {
                        try out.appendSlice(allocator, "\u{fffd}");
                        i += 1;
                        continue;
                    };
                    if (i + seq_len <= value.len and std.unicode.utf8ValidateSlice(value[i .. i + seq_len])) {
                        try out.appendSlice(allocator, value[i .. i + seq_len]);
                        i += seq_len;
                    } else {
                        try out.appendSlice(allocator, "\u{fffd}");
                        i += 1;
                    }
                }
            },
        }
    }
    return out.toOwnedSlice(allocator);
}

pub fn writeJsonString(out: *std.ArrayList(u8), allocator: std.mem.Allocator, value: []const u8) !void {
    const escaped = try jsonEscape(allocator, value);
    defer allocator.free(escaped);
    try out.append(allocator, '"');
    try out.appendSlice(allocator, escaped);
    try out.append(allocator, '"');
}

pub fn eql(a: []const u8, b: []const u8) bool {
    return std.mem.eql(u8, a, b);
}

pub fn basename(path: []const u8) []const u8 {
    return std.fs.path.basename(path);
}

pub fn envOwned(allocator: std.mem.Allocator, comptime key: []const u8) !?[]u8 {
    const ptr = std.c.getenv(key ++ "\x00") orelse return null;
    return try allocator.dupe(u8, std.mem.span(ptr));
}

pub fn envExists(comptime key: []const u8) bool {
    return std.c.getenv(key ++ "\x00") != null;
}
