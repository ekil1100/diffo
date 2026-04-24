const std = @import("std");
const diff = @import("diff.zig");

pub const ViewMode = enum {
    stacked,
    split,

    pub fn label(self: ViewMode) []const u8 {
        return switch (self) {
            .stacked => "stacked",
            .split => "split",
        };
    }
};

pub const FoldId = struct {
    file_index: usize,
    hunk_index: usize,
    ordinal: usize,

    pub fn eql(self: FoldId, other: FoldId) bool {
        return self.file_index == other.file_index and
            self.hunk_index == other.hunk_index and
            self.ordinal == other.ordinal;
    }
};

pub const FoldState = enum {
    collapsed,
    expanded,
};

pub const FoldEntry = struct {
    id: FoldId,
    state: FoldState,
};

pub const RowKind = enum {
    file_header,
    hunk_header,
    fold,
    stacked_code,
    split_code,
    file_meta,
};

pub const VisualRow = struct {
    kind: RowKind,
    hunk_index: ?usize = null,
    fold_id: ?FoldId = null,
    fold_line_count: usize = 0,
    fold_expanded: bool = false,
    change_id: ?usize = null,
    line: ?*const diff.DiffLine = null,
    left: ?*const diff.DiffLine = null,
    right: ?*const diff.DiffLine = null,
    hunk_header: ?[]const u8 = null,

    pub fn commentLine(self: VisualRow) ?*const diff.DiffLine {
        if (self.line) |line| return line;
        if (self.right) |line| return line;
        if (self.left) |line| return line;
        return null;
    }
};

pub const ChangeSpan = struct {
    id: usize,
    start_row: usize,
    end_row: usize,
};

pub const FileView = struct {
    rows: []VisualRow,
    changes: []ChangeSpan,
    additions: usize,
    deletions: usize,

    pub fn deinit(self: *FileView, allocator: std.mem.Allocator) void {
        allocator.free(self.rows);
        allocator.free(self.changes);
    }
};

const context_radius: usize = 3;
const min_fold_lines: usize = 4;

pub fn buildFileView(
    allocator: std.mem.Allocator,
    file: *const diff.DiffFile,
    file_index: usize,
    mode: ViewMode,
    folds: []const FoldEntry,
) !FileView {
    var rows: std.ArrayList(VisualRow) = .empty;
    errdefer rows.deinit(allocator);

    var additions: usize = 0;
    var deletions: usize = 0;
    for (file.hunks) |hunk| {
        for (hunk.lines) |line| {
            switch (line.kind) {
                .add => additions += 1,
                .delete => deletions += 1,
                else => {},
            }
        }
    }

    try rows.append(allocator, .{ .kind = .file_header });
    if (file.is_binary or file.hunks.len == 0) {
        try rows.append(allocator, .{ .kind = .file_meta });
    }

    for (file.hunks, 0..) |*hunk, hunk_index| {
        if (hunk.lines.len == 0) {
            try rows.append(allocator, .{ .kind = .hunk_header, .hunk_index = hunk_index, .hunk_header = hunk.header });
        }
        switch (mode) {
            .stacked => try appendStackedHunk(allocator, &rows, hunk, file_index, hunk_index, folds),
            .split => try appendSplitHunk(allocator, &rows, hunk, file_index, hunk_index, folds),
        }
    }

    var changes: std.ArrayList(ChangeSpan) = .empty;
    errdefer changes.deinit(allocator);
    assignChanges(allocator, &rows, &changes) catch |err| return err;

    return .{
        .rows = try rows.toOwnedSlice(allocator),
        .changes = try changes.toOwnedSlice(allocator),
        .additions = additions,
        .deletions = deletions,
    };
}

fn appendStackedHunk(
    allocator: std.mem.Allocator,
    rows: *std.ArrayList(VisualRow),
    hunk: *const diff.DiffHunk,
    file_index: usize,
    hunk_index: usize,
    folds: []const FoldEntry,
) !void {
    var i: usize = 0;
    var fold_ordinal: usize = 0;
    while (i < hunk.lines.len) {
        if (hunk.lines[i].kind == .context and !contextVisible(hunk.*, i)) {
            const start = i;
            while (i < hunk.lines.len and hunk.lines[i].kind == .context and !contextVisible(hunk.*, i)) : (i += 1) {}
            try appendContextRun(allocator, rows, hunk, file_index, hunk_index, fold_ordinal, start, i, folds, .stacked);
            fold_ordinal += 1;
            continue;
        }
        try rows.append(allocator, .{
            .kind = if (hunk.lines[i].kind == .meta) .file_meta else .stacked_code,
            .hunk_index = hunk_index,
            .line = &hunk.lines[i],
        });
        i += 1;
    }
}

fn appendSplitHunk(
    allocator: std.mem.Allocator,
    rows: *std.ArrayList(VisualRow),
    hunk: *const diff.DiffHunk,
    file_index: usize,
    hunk_index: usize,
    folds: []const FoldEntry,
) !void {
    var i: usize = 0;
    var fold_ordinal: usize = 0;
    while (i < hunk.lines.len) {
        if (hunk.lines[i].kind == .context) {
            if (!contextVisible(hunk.*, i)) {
                const start = i;
                while (i < hunk.lines.len and hunk.lines[i].kind == .context and !contextVisible(hunk.*, i)) : (i += 1) {}
                try appendContextRun(allocator, rows, hunk, file_index, hunk_index, fold_ordinal, start, i, folds, .split);
                fold_ordinal += 1;
            } else {
                try rows.append(allocator, .{
                    .kind = .split_code,
                    .hunk_index = hunk_index,
                    .left = &hunk.lines[i],
                    .right = &hunk.lines[i],
                });
                i += 1;
            }
            continue;
        }

        if (hunk.lines[i].kind == .delete or hunk.lines[i].kind == .add) {
            const del_start = i;
            var del_end = i;
            if (hunk.lines[i].kind == .delete) {
                while (del_end < hunk.lines.len and hunk.lines[del_end].kind == .delete) : (del_end += 1) {}
            }
            const add_start = del_end;
            var add_end = add_start;
            if (add_start < hunk.lines.len and hunk.lines[add_start].kind == .add) {
                while (add_end < hunk.lines.len and hunk.lines[add_end].kind == .add) : (add_end += 1) {}
            } else if (hunk.lines[i].kind == .add) {
                while (add_end < hunk.lines.len and hunk.lines[add_end].kind == .add) : (add_end += 1) {}
            }

            const deletes = if (hunk.lines[i].kind == .delete) hunk.lines[del_start..del_end] else hunk.lines[0..0];
            const adds = if (hunk.lines[i].kind == .delete) hunk.lines[add_start..add_end] else hunk.lines[del_start..add_end];
            const pair_count = @max(deletes.len, adds.len);
            for (0..pair_count) |pair| {
                try rows.append(allocator, .{
                    .kind = .split_code,
                    .hunk_index = hunk_index,
                    .left = if (pair < deletes.len) &deletes[pair] else null,
                    .right = if (pair < adds.len) &adds[pair] else null,
                });
            }
            i = add_end;
            continue;
        }

        try rows.append(allocator, .{
            .kind = .file_meta,
            .hunk_index = hunk_index,
            .line = &hunk.lines[i],
        });
        i += 1;
    }
}

fn appendContextRun(
    allocator: std.mem.Allocator,
    rows: *std.ArrayList(VisualRow),
    hunk: *const diff.DiffHunk,
    file_index: usize,
    hunk_index: usize,
    fold_ordinal: usize,
    start: usize,
    end: usize,
    folds: []const FoldEntry,
    mode: ViewMode,
) !void {
    if (end <= start) return;
    if (end - start < min_fold_lines) {
        for (start..end) |idx| try appendContextRow(allocator, rows, hunk, hunk_index, idx, null, mode);
        return;
    }

    const id: FoldId = .{ .file_index = file_index, .hunk_index = hunk_index, .ordinal = fold_ordinal };
    const expanded = foldState(folds, id) == .expanded;
    try rows.append(allocator, .{
        .kind = .fold,
        .hunk_index = hunk_index,
        .fold_id = id,
        .fold_line_count = end - start,
        .fold_expanded = expanded,
    });
    if (expanded) {
        for (start..end) |idx| try appendContextRow(allocator, rows, hunk, hunk_index, idx, id, mode);
    }
}

fn appendContextRow(
    allocator: std.mem.Allocator,
    rows: *std.ArrayList(VisualRow),
    hunk: *const diff.DiffHunk,
    hunk_index: usize,
    idx: usize,
    fold_id: ?FoldId,
    mode: ViewMode,
) !void {
    switch (mode) {
        .stacked => try rows.append(allocator, .{
            .kind = .stacked_code,
            .hunk_index = hunk_index,
            .fold_id = fold_id,
            .line = &hunk.lines[idx],
        }),
        .split => try rows.append(allocator, .{
            .kind = .split_code,
            .hunk_index = hunk_index,
            .fold_id = fold_id,
            .left = &hunk.lines[idx],
            .right = &hunk.lines[idx],
        }),
    }
}

fn contextVisible(hunk: diff.DiffHunk, index: usize) bool {
    if (hunk.lines[index].kind != .context) return true;
    var saw_change = false;
    var nearest: usize = std.math.maxInt(usize);
    for (hunk.lines, 0..) |line, i| {
        if (line.kind == .add or line.kind == .delete) {
            saw_change = true;
            const distance = if (i > index) i - index else index - i;
            nearest = @min(nearest, distance);
        }
    }
    return !saw_change or nearest <= context_radius;
}

pub fn foldState(folds: []const FoldEntry, id: FoldId) FoldState {
    for (folds) |entry| {
        if (entry.id.eql(id)) return entry.state;
    }
    return .collapsed;
}

fn assignChanges(
    allocator: std.mem.Allocator,
    rows: *std.ArrayList(VisualRow),
    changes: *std.ArrayList(ChangeSpan),
) !void {
    var active_start: ?usize = null;
    var id: usize = 0;
    for (rows.items, 0..) |*row, i| {
        if (isChangeRow(row.*)) {
            if (active_start == null) active_start = i;
            row.change_id = id;
        } else if (active_start) |start| {
            try changes.append(allocator, .{ .id = id, .start_row = start, .end_row = i - 1 });
            id += 1;
            active_start = null;
        }
    }
    if (active_start) |start| {
        try changes.append(allocator, .{ .id = id, .start_row = start, .end_row = rows.items.len - 1 });
    }
}

fn isChangeRow(row: VisualRow) bool {
    if (row.line) |line| return line.kind == .add or line.kind == .delete;
    if (row.left) |line| if (line.kind == .delete) return true;
    if (row.right) |line| if (line.kind == .add) return true;
    return false;
}

pub fn nextChange(view: FileView, cursor_row: usize) ?usize {
    for (view.changes) |change| {
        if (change.start_row > cursor_row) return change.start_row;
    }
    return null;
}

pub fn previousChange(view: FileView, cursor_row: usize) ?usize {
    var found: ?usize = null;
    for (view.changes) |change| {
        if (change.start_row < cursor_row) found = change.start_row;
    }
    return found;
}

test "stacked view folds long context" {
    const allocator = std.testing.allocator;
    const patch =
        \\diff --git a/a.zig b/a.zig
        \\--- a/a.zig
        \\+++ b/a.zig
        \\@@ -1,18 +1,18 @@
        \\ one
        \\ two
        \\ three
        \\ four
        \\ five
        \\ six
        \\ six-a
        \\ six-b
        \\ six-c
        \\ six-d
        \\-old();
        \\+new();
        \\ seven
        \\ eight
        \\ nine
        \\ ten
        \\ eleven
        \\ twelve
        \\ thirteen
        \\ fourteen
        \\
    ;
    const files = try diff.parsePatch(allocator, patch, .explicit);
    defer {
        for (files) |*file| file.deinit(allocator);
        allocator.free(files);
    }
    var view = try buildFileView(allocator, &files[0], 0, .stacked, &.{});
    defer view.deinit(allocator);
    var fold_count: usize = 0;
    for (view.rows) |row| {
        if (row.kind == .fold) fold_count += 1;
    }
    try std.testing.expect(fold_count >= 1);
}

test "split view pairs delete and add rows" {
    const allocator = std.testing.allocator;
    const patch =
        \\diff --git a/a.zig b/a.zig
        \\--- a/a.zig
        \\+++ b/a.zig
        \\@@ -1,2 +1,3 @@
        \\ const std = @import("std");
        \\-old();
        \\+new();
        \\+extra();
        \\
    ;
    const files = try diff.parsePatch(allocator, patch, .explicit);
    defer {
        for (files) |*file| file.deinit(allocator);
        allocator.free(files);
    }
    var view = try buildFileView(allocator, &files[0], 0, .split, &.{});
    defer view.deinit(allocator);
    var paired = false;
    for (view.rows) |row| {
        if (row.kind == .split_code and row.left != null and row.right != null and row.left.?.kind == .delete and row.right.?.kind == .add) paired = true;
    }
    try std.testing.expect(paired);
}

test "change navigation returns next and previous change starts" {
    const allocator = std.testing.allocator;
    const patch =
        \\diff --git a/a.zig b/a.zig
        \\--- a/a.zig
        \\+++ b/a.zig
        \\@@ -1,5 +1,5 @@
        \\ one
        \\-old();
        \\+new();
        \\ two
        \\-old2();
        \\+new2();
        \\
    ;
    const files = try diff.parsePatch(allocator, patch, .explicit);
    defer {
        for (files) |*file| file.deinit(allocator);
        allocator.free(files);
    }
    var view = try buildFileView(allocator, &files[0], 0, .stacked, &.{});
    defer view.deinit(allocator);
    const first = nextChange(view, 0).?;
    const second = nextChange(view, first).?;
    try std.testing.expect(second > first);
    try std.testing.expectEqual(first, previousChange(view, second).?);
}

test "expanded fold includes folded context rows" {
    const allocator = std.testing.allocator;
    const patch =
        \\diff --git a/a.zig b/a.zig
        \\--- a/a.zig
        \\+++ b/a.zig
        \\@@ -1,18 +1,18 @@
        \\ one
        \\ two
        \\ three
        \\ four
        \\ five
        \\ six
        \\ six-a
        \\ six-b
        \\ six-c
        \\ six-d
        \\-old();
        \\+new();
        \\ seven
        \\ eight
        \\ nine
        \\ ten
        \\ eleven
        \\ twelve
        \\
    ;
    const files = try diff.parsePatch(allocator, patch, .explicit);
    defer {
        for (files) |*file| file.deinit(allocator);
        allocator.free(files);
    }
    const entry: FoldEntry = .{ .id = .{ .file_index = 0, .hunk_index = 0, .ordinal = 0 }, .state = .expanded };
    var view = try buildFileView(allocator, &files[0], 0, .stacked, &.{entry});
    defer view.deinit(allocator);
    var expanded_context = false;
    for (view.rows) |row| {
        if (row.fold_id != null and row.kind == .stacked_code) expanded_context = true;
    }
    try std.testing.expect(expanded_context);
}
