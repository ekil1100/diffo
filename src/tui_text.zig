const std = @import("std");

pub fn appendCell(allocator: std.mem.Allocator, out: *std.ArrayList(u8), text: []const u8, width: usize) !void {
    const fitted = try fitCell(allocator, text, width);
    defer allocator.free(fitted);
    try out.appendSlice(allocator, fitted);
}

pub fn fitCell(allocator: std.mem.Allocator, text: []const u8, width: usize) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    var visible: usize = 0;
    var i: usize = 0;
    while (i < text.len and visible < width) {
        if (text[i] == '\x1b') {
            const start = i;
            i += 1;
            while (i < text.len and text[i] != 'm') : (i += 1) {}
            if (i < text.len) i += 1;
            try out.appendSlice(allocator, text[start..i]);
            continue;
        }
        const seq_len = utf8SeqLen(text[i]);
        if (i + seq_len > text.len) break;
        const w = displayWidth(text[i .. i + seq_len]);
        if (visible + w > width) break;
        try out.appendSlice(allocator, text[i .. i + seq_len]);
        visible += w;
        i += seq_len;
    }
    while (visible < width) : (visible += 1) try out.append(allocator, ' ');
    return out.toOwnedSlice(allocator);
}

pub fn utf8SeqLen(first: u8) usize {
    if (first < 0x80) return 1;
    if ((first & 0xe0) == 0xc0) return 2;
    if ((first & 0xf0) == 0xe0) return 3;
    if ((first & 0xf8) == 0xf0) return 4;
    return 1;
}

pub fn displayWidth(bytes: []const u8) usize {
    if (bytes.len == 0) return 0;
    if (bytes[0] < 0x20) return 0;
    if (bytes[0] < 0x80) return 1;
    const cp = std.unicode.utf8Decode(bytes) catch return 1;
    if (cp >= 0x1100 and
        (cp <= 0x115f or
            cp == 0x2329 or cp == 0x232a or
            (cp >= 0x2e80 and cp <= 0xa4cf) or
            (cp >= 0xac00 and cp <= 0xd7a3) or
            (cp >= 0xf900 and cp <= 0xfaff) or
            (cp >= 0xfe10 and cp <= 0xfe19) or
            (cp >= 0xfe30 and cp <= 0xfe6f) or
            (cp >= 0xff00 and cp <= 0xff60) or
            (cp >= 0xffe0 and cp <= 0xffe6)))
    {
        return 2;
    }
    return 1;
}

test "display width handles cjk" {
    try std.testing.expectEqual(@as(usize, 1), displayWidth("a"));
    try std.testing.expectEqual(@as(usize, 2), displayWidth("架"));
}

test "fit cell handles ansi and cjk width" {
    const allocator = std.testing.allocator;
    const fitted = try fitCell(allocator, "\x1b[31m架a\x1b[0m", 4);
    defer allocator.free(fitted);
    try std.testing.expect(std.mem.indexOf(u8, fitted, "\x1b[31m") != null);
}
