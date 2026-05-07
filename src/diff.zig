const std = @import("std");
const util = @import("util.zig");

pub const DiffLineKind = enum {
    context,
    add,
    delete,
    meta,
};

pub const DiffLine = struct {
    kind: DiffLineKind,
    old_lineno: ?u32,
    new_lineno: ?u32,
    text: []u8,
    stable_line_id: []u8,

    pub fn deinit(self: *DiffLine, allocator: std.mem.Allocator) void {
        allocator.free(self.text);
        allocator.free(self.stable_line_id);
    }
};

pub const DiffHunk = struct {
    header: []u8,
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
    lines: []DiffLine,

    pub fn deinit(self: *DiffHunk, allocator: std.mem.Allocator) void {
        allocator.free(self.header);
        for (self.lines) |*line| line.deinit(allocator);
        allocator.free(self.lines);
    }
};

pub const FileStatus = enum {
    added,
    modified,
    deleted,
    renamed,
    copied,
    binary,

    pub fn label(self: FileStatus) []const u8 {
        return switch (self) {
            .added => "A",
            .modified => "M",
            .deleted => "D",
            .renamed => "R",
            .copied => "C",
            .binary => "B",
        };
    }
};

pub const DiffSource = enum {
    unstaged,
    staged,
    untracked,
    explicit,

    pub fn label(self: DiffSource) []const u8 {
        return switch (self) {
            .unstaged => "unstaged",
            .staged => "staged",
            .untracked => "untracked",
            .explicit => "target",
        };
    }
};

pub const DiffFile = struct {
    path: []u8,
    old_path: ?[]u8,
    status: FileStatus,
    source: DiffSource,
    language: ?[]u8,
    is_binary: bool,
    hunks: []DiffHunk,
    patch_fingerprint: []u8,
    patch_text: []u8,

    pub fn deinit(self: *DiffFile, allocator: std.mem.Allocator) void {
        allocator.free(self.path);
        if (self.old_path) |old| allocator.free(old);
        if (self.language) |language| allocator.free(language);
        for (self.hunks) |*hunk| hunk.deinit(allocator);
        allocator.free(self.hunks);
        allocator.free(self.patch_fingerprint);
        allocator.free(self.patch_text);
    }

    pub fn lineCount(self: DiffFile) usize {
        var count: usize = 0;
        for (self.hunks) |hunk| count += 1 + hunk.lines.len;
        return count;
    }
};

pub const ReviewTargetKind = enum {
    working_tree,
    cached,
    commit,
    range,
    symmetric_range,
};

pub const ReviewTarget = struct {
    kind: ReviewTargetKind,
    raw_args: []const []const u8,
    normalized_spec: []u8,
    target_id: []u8,

    pub fn deinit(self: *ReviewTarget, allocator: std.mem.Allocator) void {
        allocator.free(self.normalized_spec);
        allocator.free(self.target_id);
    }
};

pub const Repository = struct {
    root_path: []u8,
    repo_id: []u8,
    current_branch: []u8,

    pub fn deinit(self: *Repository, allocator: std.mem.Allocator) void {
        allocator.free(self.root_path);
        allocator.free(self.repo_id);
        allocator.free(self.current_branch);
    }
};

pub const DiffSnapshot = struct {
    snapshot_id: []u8,
    repository: Repository,
    review_target: ReviewTarget,
    files: []DiffFile,

    pub fn deinit(self: *DiffSnapshot, allocator: std.mem.Allocator) void {
        allocator.free(self.snapshot_id);
        self.repository.deinit(allocator);
        self.review_target.deinit(allocator);
        for (self.files) |*file| file.deinit(allocator);
        allocator.free(self.files);
    }

    pub fn totalLines(self: DiffSnapshot) usize {
        var count: usize = 0;
        for (self.files) |file| count += file.lineCount();
        return count;
    }
};

pub const ParseError = error{
    ParseFailed,
} || std.mem.Allocator.Error;

pub fn makeReviewTarget(allocator: std.mem.Allocator, args: []const []const u8) !ReviewTarget {
    const normalized = try util.joinArgs(allocator, args);
    errdefer allocator.free(normalized);

    var kind: ReviewTargetKind = if (args.len == 0) .working_tree else .commit;
    for (args) |arg| {
        if (util.eql(arg, "--cached") or util.eql(arg, "--staged")) {
            kind = .cached;
            break;
        }
        if (util.contains(arg, "...")) {
            kind = .symmetric_range;
            break;
        }
        if (util.contains(arg, "..")) {
            kind = .range;
            break;
        }
    }

    const id_input = try std.fmt.allocPrint(allocator, "{s}:{s}", .{ @tagName(kind), normalized });
    defer allocator.free(id_input);
    const id_hash = try util.hashHex(allocator, id_input);
    errdefer allocator.free(id_hash);
    const id = try std.fmt.allocPrint(allocator, "target_{s}", .{id_hash[0..16]});
    allocator.free(id_hash);

    return .{
        .kind = kind,
        .raw_args = args,
        .normalized_spec = normalized,
        .target_id = id,
    };
}

pub fn parsePatch(allocator: std.mem.Allocator, patch: []const u8, source: DiffSource) ![]DiffFile {
    var files: std.ArrayList(DiffFile) = .empty;
    var pos: usize = 0;
    while (pos < patch.len) {
        const start_rel = findDiffStart(patch[pos..]) orelse break;
        const start = pos + start_rel;
        const next_rel = findNextDiffStart(patch[start + 1 ..]);
        const end = if (next_rel) |rel| start + 1 + rel else patch.len;
        const section = std.mem.trim(u8, patch[start..end], "\n");
        if (section.len > 0) {
            const file = try parseFilePatch(allocator, section, source);
            try files.append(allocator, file);
        }
        pos = end;
    }
    return files.toOwnedSlice(allocator);
}

fn findDiffStart(bytes: []const u8) ?usize {
    if (std.mem.startsWith(u8, bytes, "diff --git ")) return 0;
    if (std.mem.indexOf(u8, bytes, "\ndiff --git ")) |idx| return idx + 1;
    return null;
}

fn findNextDiffStart(bytes: []const u8) ?usize {
    return findDiffStart(bytes);
}

fn parseFilePatch(allocator: std.mem.Allocator, patch: []const u8, source: DiffSource) ParseError!DiffFile {
    var path: ?[]u8 = null;
    var old_path: ?[]u8 = null;
    var status: FileStatus = .modified;
    var is_binary = false;
    var hunks: std.ArrayList(DiffHunk) = .empty;
    var current_lines: std.ArrayList(DiffLine) = .empty;
    var current_header: ?[]u8 = null;
    var old_start: u32 = 0;
    var old_count: u32 = 0;
    var new_start: u32 = 0;
    var new_count: u32 = 0;
    var old_line: u32 = 0;
    var new_line: u32 = 0;

    errdefer {
        if (path) |p| allocator.free(p);
        if (old_path) |p| allocator.free(p);
        if (current_header) |h| allocator.free(h);
        for (current_lines.items) |*line| line.deinit(allocator);
        current_lines.deinit(allocator);
        for (hunks.items) |*hunk| hunk.deinit(allocator);
        hunks.deinit(allocator);
    }

    var lines = std.mem.splitScalar(u8, patch, '\n');
    while (lines.next()) |raw_line| {
        const line = util.trimLine(raw_line);
        if (util.startsWith(line, "diff --git ")) {
            const parsed = try parseDiffGitPath(allocator, line);
            if (old_path) |old| allocator.free(old);
            old_path = parsed.old_path;
            if (path) |p| allocator.free(p);
            path = parsed.path;
        } else if (util.startsWith(line, "new file mode")) {
            status = .added;
        } else if (util.startsWith(line, "deleted file mode")) {
            status = .deleted;
        } else if (util.startsWith(line, "rename from ")) {
            status = .renamed;
            if (old_path) |old| allocator.free(old);
            old_path = try util.dupe(allocator, line["rename from ".len..]);
        } else if (util.startsWith(line, "copy from ")) {
            status = .copied;
            if (old_path) |old| allocator.free(old);
            old_path = try util.dupe(allocator, line["copy from ".len..]);
        } else if (util.startsWith(line, "rename to ")) {
            status = .renamed;
            if (path) |p| allocator.free(p);
            path = try util.dupe(allocator, line["rename to ".len..]);
        } else if (util.startsWith(line, "copy to ")) {
            status = .copied;
            if (path) |p| allocator.free(p);
            path = try util.dupe(allocator, line["copy to ".len..]);
        } else if (util.startsWith(line, "Binary files ") or util.startsWith(line, "GIT binary patch")) {
            status = .binary;
            is_binary = true;
        } else if (util.startsWith(line, "+++ ")) {
            if (!util.eql(line[4..], "/dev/null")) {
                const new_path = stripGitPrefix(line[4..]);
                if (path) |p| allocator.free(p);
                path = try util.dupe(allocator, new_path);
            }
        } else if (util.startsWith(line, "--- ")) {
            if (!util.eql(line[4..], "/dev/null")) {
                const old = stripGitPrefix(line[4..]);
                if (old_path) |p| allocator.free(p);
                old_path = try util.dupe(allocator, old);
            }
        } else if (util.startsWith(line, "@@ ")) {
            if (current_header) |header| {
                const owned_lines = try current_lines.toOwnedSlice(allocator);
                try hunks.append(allocator, .{
                    .header = header,
                    .old_start = old_start,
                    .old_count = old_count,
                    .new_start = new_start,
                    .new_count = new_count,
                    .lines = owned_lines,
                });
                current_lines = .empty;
            }
            const parsed = parseHunkHeader(line) orelse return error.ParseFailed;
            current_header = try util.dupe(allocator, line);
            old_start = parsed.old_start;
            old_count = parsed.old_count;
            new_start = parsed.new_start;
            new_count = parsed.new_count;
            old_line = old_start;
            new_line = new_start;
        } else if (current_header != null) {
            const kind: DiffLineKind = if (line.len == 0)
                .context
            else switch (line[0]) {
                '+' => .add,
                '-' => .delete,
                ' ' => .context,
                '\\' => .meta,
                else => .context,
            };
            const text = if (line.len > 0 and (line[0] == '+' or line[0] == '-' or line[0] == ' ')) line[1..] else line;
            var old_lineno: ?u32 = null;
            var new_lineno: ?u32 = null;
            switch (kind) {
                .add => {
                    new_lineno = new_line;
                    new_line += 1;
                },
                .delete => {
                    old_lineno = old_line;
                    old_line += 1;
                },
                .context => {
                    old_lineno = old_line;
                    new_lineno = new_line;
                    old_line += 1;
                    new_line += 1;
                },
                .meta => {},
            }
            const line_id_input = try std.fmt.allocPrint(allocator, "{s}:{s}:{?d}:{?d}:{s}", .{
                path orelse "",
                @tagName(kind),
                old_lineno,
                new_lineno,
                text,
            });
            defer allocator.free(line_id_input);
            const id_hash = try util.hashHex(allocator, line_id_input);
            errdefer allocator.free(id_hash);
            const stable_id = try std.fmt.allocPrint(allocator, "line_{s}", .{id_hash[0..16]});
            allocator.free(id_hash);

            try current_lines.append(allocator, .{
                .kind = kind,
                .old_lineno = old_lineno,
                .new_lineno = new_lineno,
                .text = try util.dupe(allocator, text),
                .stable_line_id = stable_id,
            });
        }
    }

    if (current_header) |header| {
        current_header = null;
        const owned_lines = try current_lines.toOwnedSlice(allocator);
        try hunks.append(allocator, .{
            .header = header,
            .old_start = old_start,
            .old_count = old_count,
            .new_start = new_start,
            .new_count = new_count,
            .lines = owned_lines,
        });
        current_lines = .empty;
    }

    const final_path = path orelse try util.dupe(allocator, "(unknown)");
    path = null;
    const patch_copy = try util.dupe(allocator, patch);
    errdefer allocator.free(patch_copy);
    const fingerprint_hash = try util.hashHex(allocator, patch);
    errdefer allocator.free(fingerprint_hash);
    const fingerprint = try std.fmt.allocPrint(allocator, "sha256:{s}", .{fingerprint_hash});
    allocator.free(fingerprint_hash);
    errdefer allocator.free(fingerprint);
    const language = detectLanguage(allocator, final_path) catch null;
    errdefer if (language) |lang| allocator.free(lang);

    return .{
        .path = final_path,
        .old_path = old_path,
        .status = status,
        .source = source,
        .language = language,
        .is_binary = is_binary,
        .hunks = try hunks.toOwnedSlice(allocator),
        .patch_fingerprint = fingerprint,
        .patch_text = patch_copy,
    };
}

const ParsedPath = struct {
    old_path: ?[]u8,
    path: []u8,
};

fn parseDiffGitPath(allocator: std.mem.Allocator, line: []const u8) !ParsedPath {
    const rest = line["diff --git ".len..];
    const marker = " b/";
    if (std.mem.indexOf(u8, rest, marker)) |idx| {
        const old_raw = stripGitPrefix(rest[0..idx]);
        const new_raw = stripGitPrefix(rest[idx + 1 ..]);
        return .{
            .old_path = try util.dupe(allocator, old_raw),
            .path = try util.dupe(allocator, new_raw),
        };
    }
    return .{
        .old_path = null,
        .path = try util.dupe(allocator, rest),
    };
}

fn stripGitPrefix(path: []const u8) []const u8 {
    if (std.mem.startsWith(u8, path, "a/") or std.mem.startsWith(u8, path, "b/")) return path[2..];
    return path;
}

const ParsedHunk = struct {
    old_start: u32,
    old_count: u32,
    new_start: u32,
    new_count: u32,
};

fn parseHunkHeader(header: []const u8) ?ParsedHunk {
    const first_space = std.mem.indexOfScalarPos(u8, header, 3, ' ') orelse return null;
    const second_space = std.mem.indexOfScalarPos(u8, header, first_space + 1, ' ') orelse return null;
    const old_part = header[3..first_space];
    const new_part = header[first_space + 1 .. second_space];
    if (old_part.len == 0 or new_part.len == 0 or old_part[0] != '-' or new_part[0] != '+') return null;
    const old_range = parseRange(old_part[1..]) orelse return null;
    const new_range = parseRange(new_part[1..]) orelse return null;
    return .{
        .old_start = old_range.start,
        .old_count = old_range.count,
        .new_start = new_range.start,
        .new_count = new_range.count,
    };
}

const ParsedRange = struct {
    start: u32,
    count: u32,
};

fn parseRange(part: []const u8) ?ParsedRange {
    if (std.mem.indexOfScalar(u8, part, ',')) |idx| {
        return .{
            .start = std.fmt.parseInt(u32, part[0..idx], 10) catch return null,
            .count = std.fmt.parseInt(u32, part[idx + 1 ..], 10) catch return null,
        };
    }
    return .{
        .start = std.fmt.parseInt(u32, part, 10) catch return null,
        .count = 1,
    };
}

fn detectLanguage(allocator: std.mem.Allocator, path: []const u8) !?[]u8 {
    const base = util.basename(path);
    if (util.eql(base, "Makefile")) return try util.dupe(allocator, "make");
    if (util.eql(base, "Dockerfile")) return try util.dupe(allocator, "dockerfile");
    if (util.eql(base, "build.zig")) return try util.dupe(allocator, "zig");
    const ext = std.fs.path.extension(path);
    if (ext.len == 0) return null;
    const lang = if (util.eql(ext, ".zig"))
        "zig"
    else if (util.eql(ext, ".tsx"))
        "tsx"
    else if (util.eql(ext, ".ts") or util.eql(ext, ".mts") or util.eql(ext, ".cts"))
        "typescript"
    else if (util.eql(ext, ".js") or util.eql(ext, ".jsx") or util.eql(ext, ".mjs") or util.eql(ext, ".cjs"))
        "javascript"
    else if (util.eql(ext, ".py") or util.eql(ext, ".pyw"))
        "python"
    else if (util.eql(ext, ".go"))
        "go"
    else if (util.eql(ext, ".rs"))
        "rust"
    else if (util.eql(ext, ".c") or util.eql(ext, ".h"))
        "c"
    else if (util.eql(ext, ".cpp") or util.eql(ext, ".hpp") or util.eql(ext, ".cc") or util.eql(ext, ".cxx") or util.eql(ext, ".hxx") or util.eql(ext, ".hh"))
        "cpp"
    else if (util.eql(ext, ".java"))
        "java"
    else if (util.eql(ext, ".md"))
        "markdown"
    else if (util.eql(ext, ".json"))
        "json"
    else if (util.eql(ext, ".html"))
        "html"
    else if (util.eql(ext, ".css"))
        "css"
    else
        return null;
    return try util.dupe(allocator, lang);
}

test "detects languages requested for highlighting" {
    const allocator = std.testing.allocator;
    const samples = [_]struct {
        path: []const u8,
        language: []const u8,
    }{
        .{ .path = "src/app.ts", .language = "typescript" },
        .{ .path = "src/app.tsx", .language = "tsx" },
        .{ .path = "src/app.mts", .language = "typescript" },
        .{ .path = "src/app.js", .language = "javascript" },
        .{ .path = "src/app.cjs", .language = "javascript" },
        .{ .path = "src/lib.rs", .language = "rust" },
        .{ .path = "src/main.c", .language = "c" },
        .{ .path = "include/main.h", .language = "c" },
        .{ .path = "src/main.cpp", .language = "cpp" },
        .{ .path = "include/main.hh", .language = "cpp" },
        .{ .path = "scripts/tool.py", .language = "python" },
        .{ .path = "scripts/tool.pyw", .language = "python" },
    };
    for (samples) |sample| {
        const language = (try detectLanguage(allocator, sample.path)) orelse return error.TestExpectedEqual;
        defer allocator.free(language);
        try std.testing.expect(util.eql(language, sample.language));
    }
}

pub fn findLineByFlatIndex(file: DiffFile, index: usize) ?struct { hunk_index: usize, line_index: ?usize, line: ?*const DiffLine, header: []const u8 } {
    var cursor: usize = 0;
    for (file.hunks, 0..) |*hunk, hunk_index| {
        if (cursor == index) return .{ .hunk_index = hunk_index, .line_index = null, .line = null, .header = hunk.header };
        cursor += 1;
        for (hunk.lines, 0..) |*line, line_index| {
            if (cursor == index) return .{ .hunk_index = hunk_index, .line_index = line_index, .line = line, .header = hunk.header };
            cursor += 1;
        }
    }
    return null;
}

test "parse unified diff file" {
    const allocator = std.testing.allocator;
    const patch =
        \\diff --git a/src/main.zig b/src/main.zig
        \\index 1111111..2222222 100644
        \\--- a/src/main.zig
        \\+++ b/src/main.zig
        \\@@ -1,2 +1,3 @@
        \\ const std = @import("std");
        \\-old();
        \\+new();
        \\+extra();
        \\
    ;
    const files = try parsePatch(allocator, patch, .explicit);
    defer {
        for (files) |*file| file.deinit(allocator);
        allocator.free(files);
    }
    try std.testing.expectEqual(@as(usize, 1), files.len);
    try std.testing.expectEqualStrings("src/main.zig", files[0].path);
    try std.testing.expectEqual(@as(usize, 1), files[0].hunks.len);
    try std.testing.expectEqual(@as(usize, 4), files[0].hunks[0].lines.len);
    try std.testing.expectEqual(DiffLineKind.add, files[0].hunks[0].lines[2].kind);
}

test "review target kind" {
    const allocator = std.testing.allocator;
    var args = [_][]const u8{ "main...feature", "--", "src" };
    var target = try makeReviewTarget(allocator, &args);
    defer target.deinit(allocator);
    try std.testing.expectEqual(ReviewTargetKind.symmetric_range, target.kind);
}
