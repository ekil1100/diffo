const std = @import("std");

pub fn appendCell(allocator: std.mem.Allocator, out: *std.ArrayList(u8), text: []const u8, width: usize) !void {
    const fitted = try fitCell(allocator, text, width);
    defer allocator.free(fitted);
    try out.appendSlice(allocator, fitted);
}

pub fn fitCell(allocator: std.mem.Allocator, text: []const u8, width: usize) ![]u8 {
    var out: std.ArrayList(u8) = .empty;
    errdefer out.deinit(allocator);
    var visible: usize = 0;
    var i: usize = 0;
    while (i < text.len and visible < width) {
        if (text[i] == 0x1b) {
            const seq_len = ansiSeqLen(text, i);
            const seq = text[i .. i + seq_len];
            // Forward only SGR (styling) sequences; drop cursor moves, clears, OSC, etc.
            // Every escape is zero display width, so visible is unchanged either way.
            if (isSgr(seq)) try out.appendSlice(allocator, seq);
            i += seq_len;
            continue;
        }
        if (text[i] < 0x20 or text[i] == 0x7f) {
            // Replace a raw control byte (tab, CR, BEL, DEL, ...) with a visible
            // placeholder so width accounting stays correct and the terminal never
            // executes a control embedded in untrusted file/path/comment text.
            try out.append(allocator, ' ');
            visible += 1;
            i += 1;
            continue;
        }
        const seq_len = utf8SeqLen(text[i]);
        if (i + seq_len > text.len) break;
        const w = cellWidth(text[i .. i + seq_len]);
        if (visible + w > width) break;
        try out.appendSlice(allocator, text[i .. i + seq_len]);
        visible += w;
        i += seq_len;
    }
    while (visible < width) : (visible += 1) try out.append(allocator, ' ');
    return out.toOwnedSlice(allocator);
}

/// Byte length of the ANSI escape sequence starting at text[i] (text[i] must be 0x1b).
/// Handles CSI (ESC [ params intermediates final) and OSC (ESC ] ... BEL/ST); other forms
/// consume ESC plus one byte. The whole sequence has zero display width.
pub fn ansiSeqLen(text: []const u8, i: usize) usize {
    if (i + 1 >= text.len) return 1; // lone ESC at end
    switch (text[i + 1]) {
        '[' => {
            var j = i + 2;
            while (j < text.len and text[j] >= 0x30 and text[j] <= 0x3f) : (j += 1) {} // parameters
            while (j < text.len and text[j] >= 0x20 and text[j] <= 0x2f) : (j += 1) {} // intermediates
            if (j < text.len) j += 1; // final byte 0x40-0x7e
            return j - i;
        },
        ']' => {
            var j = i + 2;
            while (j < text.len) : (j += 1) {
                if (text[j] == 0x07) return j + 1 - i; // BEL terminator
                if (text[j] == 0x1b and j + 1 < text.len and text[j + 1] == '\\') return j + 2 - i; // ST
            }
            return text.len - i;
        },
        else => return 2,
    }
}

/// Whether an escape sequence (as bounded by ansiSeqLen) is an SGR colour/style sequence.
pub fn isSgr(seq: []const u8) bool {
    return seq.len >= 3 and seq[0] == 0x1b and seq[1] == '[' and seq[seq.len - 1] == 'm';
}

/// Canonical visual width of one UTF-8 sequence. Single source of truth shared by the cell
/// fitter and the wrap routines so they never disagree on zero-width bytes.
pub fn cellWidth(bytes: []const u8) usize {
    return displayWidth(bytes);
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
    if (bytes[0] < 0x20 or bytes[0] == 0x7f) return 0; // control; callers replace with a placeholder
    if (bytes[0] < 0x80) return 1;
    const cp = std.unicode.utf8Decode(bytes) catch return 1;
    if (isZeroWidthCp(cp)) return 0;
    if (isWideCp(cp)) return 2;
    return 1;
}

fn isZeroWidthCp(cp: u21) bool {
    return (cp >= 0x0300 and cp <= 0x036f) or // combining diacritical marks
        (cp >= 0x1ab0 and cp <= 0x1aff) or // combining diacritical marks extended
        (cp >= 0x1dc0 and cp <= 0x1dff) or // combining diacritical marks supplement
        (cp >= 0x20d0 and cp <= 0x20ff) or // combining marks for symbols
        (cp >= 0xfe20 and cp <= 0xfe2f) or // combining half marks
        cp == 0x200b or cp == 0x200c or cp == 0x200d or cp == 0xfeff; // ZWSP/ZWNJ/ZWJ/BOM
}

fn isWideCp(cp: u21) bool {
    return (cp >= 0x1100 and cp <= 0x115f) or // Hangul Jamo
        cp == 0x2329 or cp == 0x232a or
        (cp >= 0x2e80 and cp <= 0xa4cf) or // CJK radicals .. Yi
        (cp >= 0xac00 and cp <= 0xd7a3) or // Hangul syllables
        (cp >= 0xf900 and cp <= 0xfaff) or // CJK compatibility ideographs
        (cp >= 0xfe10 and cp <= 0xfe19) or
        (cp >= 0xfe30 and cp <= 0xfe6f) or
        (cp >= 0xff00 and cp <= 0xff60) or // fullwidth forms
        (cp >= 0xffe0 and cp <= 0xffe6) or
        (cp >= 0x1f300 and cp <= 0x1faff) or // emoji & pictographs
        (cp >= 0x20000 and cp <= 0x3fffd); // CJK extension B and beyond
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

test "display width handles combining marks and emoji" {
    try std.testing.expectEqual(@as(usize, 0), displayWidth("\u{0301}")); // combining acute
    try std.testing.expectEqual(@as(usize, 0), displayWidth("\u{200d}")); // zero-width joiner
    try std.testing.expectEqual(@as(usize, 2), displayWidth("\u{1f600}")); // emoji
    try std.testing.expectEqual(@as(usize, 0), displayWidth("\t"));
}

test "fit cell replaces control bytes and drops non-sgr escapes" {
    const allocator = std.testing.allocator;
    // The tab becomes one placeholder cell; the clear-line escape is zero width and dropped
    // (so b and c sit adjacent), and nothing control-related reaches the output.
    const fitted = try fitCell(allocator, "a\tb\x1b[2Kc", 4);
    defer allocator.free(fitted);
    try std.testing.expectEqualStrings("a bc", fitted);
}

test "fit cell forwards sgr but strips osc" {
    const allocator = std.testing.allocator;
    const fitted = try fitCell(allocator, "\x1b[31mx\x1b]0;evil\x07y", 4);
    defer allocator.free(fitted);
    try std.testing.expect(std.mem.indexOf(u8, fitted, "\x1b[31m") != null);
    try std.testing.expect(std.mem.indexOf(u8, fitted, "\x1b]0;") == null);
    try std.testing.expect(std.mem.indexOf(u8, fitted, "evil") == null);
}

test "ansi seq len bounds csi osc and lone escape" {
    try std.testing.expectEqual(@as(usize, 5), ansiSeqLen("\x1b[31m", 0));
    try std.testing.expectEqual(@as(usize, 4), ansiSeqLen("\x1b[2K", 0));
    try std.testing.expectEqual(@as(usize, 1), ansiSeqLen("\x1b", 0));
    try std.testing.expectEqual(@as(usize, 7), ansiSeqLen("\x1b]0;hi\x07", 0)); // ESC ] 0 ; h i BEL
}
