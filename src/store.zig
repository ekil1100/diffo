const std = @import("std");
const diff = @import("diff.zig");
const util = @import("util.zig");

pub const StoreError = error{
    StorageCorrupted,
    StorageWriteFailed,
} || std.mem.Allocator.Error;

pub const MatchStatus = enum {
    exact,
    relocated,
    stale,
    missing,

    pub fn label(self: MatchStatus) []const u8 {
        return @tagName(self);
    }
};

pub const Comment = struct {
    comment_id: []u8,
    repository_id: []u8,
    review_target_id: []u8,
    file_path: []u8,
    side: []u8,
    start_line: u32,
    end_line: u32,
    stable_line_id: []u8,
    hunk_header: []u8,
    context_before: []u8,
    context_after: []u8,
    patch_fingerprint: []u8,
    match_status: MatchStatus,
    body: []u8,
    author: []u8,
    created_at: []u8,
    updated_at: []u8,

    pub fn deinit(self: *Comment, allocator: std.mem.Allocator) void {
        allocator.free(self.comment_id);
        allocator.free(self.repository_id);
        allocator.free(self.review_target_id);
        allocator.free(self.file_path);
        allocator.free(self.side);
        allocator.free(self.stable_line_id);
        allocator.free(self.hunk_header);
        allocator.free(self.context_before);
        allocator.free(self.context_after);
        allocator.free(self.patch_fingerprint);
        allocator.free(self.body);
        allocator.free(self.author);
        allocator.free(self.created_at);
        allocator.free(self.updated_at);
    }
};

pub const ReviewState = struct {
    repository_id: []u8,
    review_target_id: []u8,
    file_path: []u8,
    status: []u8,
    patch_fingerprint: []u8,
    updated_at: []u8,

    pub fn deinit(self: *ReviewState, allocator: std.mem.Allocator) void {
        allocator.free(self.repository_id);
        allocator.free(self.review_target_id);
        allocator.free(self.file_path);
        allocator.free(self.status);
        allocator.free(self.patch_fingerprint);
        allocator.free(self.updated_at);
    }
};

pub const Store = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    repo_dir: []u8,
    comments_path: []u8,
    states_path: []u8,
    comments: []Comment,
    states: []ReviewState,

    pub fn init(allocator: std.mem.Allocator, io: std.Io, repo_id: []const u8) StoreError!Store {
        const base = try stateBaseDir(allocator);
        defer allocator.free(base);
        const repo_dir = try std.fmt.allocPrint(allocator, "{s}/repos/{s}", .{ base, repo_id });
        errdefer allocator.free(repo_dir);
        std.Io.Dir.cwd().createDirPath(io, repo_dir) catch return error.StorageWriteFailed;

        const comments_path = try std.fmt.allocPrint(allocator, "{s}/comments.json", .{repo_dir});
        errdefer allocator.free(comments_path);
        const states_path = try std.fmt.allocPrint(allocator, "{s}/review-states.json", .{repo_dir});
        errdefer allocator.free(states_path);

        return .{
            .allocator = allocator,
            .io = io,
            .repo_dir = repo_dir,
            .comments_path = comments_path,
            .states_path = states_path,
            .comments = try loadComments(allocator, io, comments_path),
            .states = try loadStates(allocator, io, states_path),
        };
    }

    pub fn deinit(self: *Store) void {
        self.allocator.free(self.repo_dir);
        self.allocator.free(self.comments_path);
        self.allocator.free(self.states_path);
        for (self.comments) |*comment| comment.deinit(self.allocator);
        self.allocator.free(self.comments);
        for (self.states) |*state| state.deinit(self.allocator);
        self.allocator.free(self.states);
    }

    pub fn save(self: *Store) StoreError!void {
        try self.saveComments();
        try self.saveStates();
    }

    pub fn commentCount(self: Store, file_path: []const u8) usize {
        var count: usize = 0;
        for (self.comments) |comment| {
            if (util.eql(comment.file_path, file_path)) count += 1;
        }
        return count;
    }

    pub fn isReviewed(self: Store, file_path: []const u8, patch_fingerprint: []const u8, target_id: []const u8) bool {
        for (self.states) |state| {
            if (util.eql(state.file_path, file_path) and
                util.eql(state.review_target_id, target_id) and
                util.eql(state.status, "reviewed") and
                util.eql(state.patch_fingerprint, patch_fingerprint))
            {
                return true;
            }
        }
        return false;
    }

    pub fn statusForFile(self: Store, file_path: []const u8, patch_fingerprint: []const u8, target_id: []const u8) []const u8 {
        return if (self.isReviewed(file_path, patch_fingerprint, target_id)) "reviewed" else "unreviewed";
    }

    pub fn setReviewed(self: *Store, repository_id: []const u8, target_id: []const u8, file: diff.DiffFile, reviewed: bool) StoreError!void {
        const now = try util.nowIso(self.allocator, self.io);
        defer self.allocator.free(now);
        for (self.states) |*state| {
            if (util.eql(state.file_path, file.path) and util.eql(state.review_target_id, target_id)) {
                self.allocator.free(state.status);
                self.allocator.free(state.patch_fingerprint);
                self.allocator.free(state.updated_at);
                state.status = try util.dupe(self.allocator, if (reviewed) "reviewed" else "unreviewed");
                state.patch_fingerprint = try util.dupe(self.allocator, file.patch_fingerprint);
                state.updated_at = try util.dupe(self.allocator, now);
                try self.saveStates();
                return;
            }
        }

        var list: std.ArrayList(ReviewState) = .empty;
        defer list.deinit(self.allocator);
        try list.appendSlice(self.allocator, self.states);
        try list.append(self.allocator, .{
            .repository_id = try util.dupe(self.allocator, repository_id),
            .review_target_id = try util.dupe(self.allocator, target_id),
            .file_path = try util.dupe(self.allocator, file.path),
            .status = try util.dupe(self.allocator, if (reviewed) "reviewed" else "unreviewed"),
            .patch_fingerprint = try util.dupe(self.allocator, file.patch_fingerprint),
            .updated_at = try util.dupe(self.allocator, now),
        });
        self.allocator.free(self.states);
        self.states = try list.toOwnedSlice(self.allocator);
        try self.saveStates();
    }

    pub fn addComment(
        self: *Store,
        repository_id: []const u8,
        target_id: []const u8,
        file: diff.DiffFile,
        line: *const diff.DiffLine,
        hunk_header: []const u8,
        end_line: u32,
        body: []const u8,
        author: []const u8,
    ) StoreError!Comment {
        const now = try util.nowIso(self.allocator, self.io);
        defer self.allocator.free(now);
        const side = if (line.kind == .delete) "old" else "new";
        const start_line = if (line.kind == .delete) (line.old_lineno orelse 0) else (line.new_lineno orelse line.old_lineno orelse 0);
        const id_input = try std.fmt.allocPrint(self.allocator, "{s}:{s}:{s}:{d}:{d}:{s}:{s}", .{
            repository_id,
            target_id,
            file.path,
            start_line,
            end_line,
            now,
            body,
        });
        defer self.allocator.free(id_input);
        const id_hash = try util.hashHex(self.allocator, id_input);
        defer self.allocator.free(id_hash);
        const comment_id = try std.fmt.allocPrint(self.allocator, "cmt_{s}", .{id_hash[0..16]});

        var comment = Comment{
            .comment_id = comment_id,
            .repository_id = try util.dupe(self.allocator, repository_id),
            .review_target_id = try util.dupe(self.allocator, target_id),
            .file_path = try util.dupe(self.allocator, file.path),
            .side = try util.dupe(self.allocator, side),
            .start_line = start_line,
            .end_line = if (end_line == 0) start_line else end_line,
            .stable_line_id = try util.dupe(self.allocator, line.stable_line_id),
            .hunk_header = try util.dupe(self.allocator, hunk_header),
            .context_before = try util.dupe(self.allocator, ""),
            .context_after = try util.dupe(self.allocator, ""),
            .patch_fingerprint = try util.dupe(self.allocator, file.patch_fingerprint),
            .match_status = .exact,
            .body = try util.dupe(self.allocator, body),
            .author = try util.dupe(self.allocator, author),
            .created_at = try util.dupe(self.allocator, now),
            .updated_at = try util.dupe(self.allocator, now),
        };
        errdefer comment.deinit(self.allocator);

        var list: std.ArrayList(Comment) = .empty;
        defer list.deinit(self.allocator);
        try list.appendSlice(self.allocator, self.comments);
        try list.append(self.allocator, comment);
        self.allocator.free(self.comments);
        self.comments = try list.toOwnedSlice(self.allocator);
        try self.saveComments();
        return cloneComment(self.allocator, comment);
    }

    pub fn refreshMatchStatus(self: *Store, snapshot: diff.DiffSnapshot) void {
        for (self.comments) |*comment| {
            var found_file = false;
            for (snapshot.files) |file| {
                if (!util.eql(file.path, comment.file_path)) continue;
                found_file = true;
                comment.match_status = if (util.eql(file.patch_fingerprint, comment.patch_fingerprint)) .exact else .stale;
                break;
            }
            if (!found_file) comment.match_status = .missing;
        }
    }

    fn saveComments(self: *Store) StoreError!void {
        var out: std.ArrayList(u8) = .empty;
        defer out.deinit(self.allocator);
        try out.appendSlice(self.allocator, "{\n  \"schema_version\": 1,\n  \"comments\": [\n");
        for (self.comments, 0..) |comment, i| {
            if (i > 0) try out.appendSlice(self.allocator, ",\n");
            try out.appendSlice(self.allocator, "    {\n");
            try writeJsonField(&out, self.allocator, "comment_id", comment.comment_id, true);
            try writeJsonField(&out, self.allocator, "repository_id", comment.repository_id, true);
            try writeJsonField(&out, self.allocator, "review_target_id", comment.review_target_id, true);
            try writeJsonField(&out, self.allocator, "file_path", comment.file_path, true);
            try out.appendSlice(self.allocator, "      \"anchor\": {\n");
            try writeJsonField(&out, self.allocator, "side", comment.side, true);
            try writeIntField(&out, self.allocator, "start_line", comment.start_line, true);
            try writeIntField(&out, self.allocator, "end_line", comment.end_line, true);
            try out.appendSlice(self.allocator, "        \"stable_line_ids\": [");
            try util.writeJsonString(&out, self.allocator, comment.stable_line_id);
            try out.appendSlice(self.allocator, "],\n");
            try writeJsonField(&out, self.allocator, "hunk_header", comment.hunk_header, true);
            try out.appendSlice(self.allocator, "        \"context_before\": [],\n");
            try out.appendSlice(self.allocator, "        \"context_after\": [],\n");
            try writeJsonField(&out, self.allocator, "patch_fingerprint", comment.patch_fingerprint, true);
            try writeJsonField(&out, self.allocator, "match_status", comment.match_status.label(), false);
            try out.appendSlice(self.allocator, "      },\n");
            try writeJsonField(&out, self.allocator, "body", comment.body, true);
            try writeJsonField(&out, self.allocator, "author", comment.author, true);
            try writeJsonField(&out, self.allocator, "created_at", comment.created_at, true);
            try writeJsonField(&out, self.allocator, "updated_at", comment.updated_at, false);
            try out.appendSlice(self.allocator, "    }");
        }
        try out.appendSlice(self.allocator, "\n  ]\n}\n");
        try atomicWrite(self.allocator, self.io, self.comments_path, out.items);
    }

    fn saveStates(self: *Store) StoreError!void {
        var out: std.ArrayList(u8) = .empty;
        defer out.deinit(self.allocator);
        try out.appendSlice(self.allocator, "{\n  \"schema_version\": 1,\n  \"states\": [\n");
        for (self.states, 0..) |state, i| {
            if (i > 0) try out.appendSlice(self.allocator, ",\n");
            try out.appendSlice(self.allocator, "    {\n");
            try writeJsonField(&out, self.allocator, "repository_id", state.repository_id, true);
            try writeJsonField(&out, self.allocator, "review_target_id", state.review_target_id, true);
            try writeJsonField(&out, self.allocator, "file_path", state.file_path, true);
            try writeJsonField(&out, self.allocator, "status", state.status, true);
            try writeJsonField(&out, self.allocator, "patch_fingerprint", state.patch_fingerprint, true);
            try writeJsonField(&out, self.allocator, "updated_at", state.updated_at, false);
            try out.appendSlice(self.allocator, "    }");
        }
        try out.appendSlice(self.allocator, "\n  ]\n}\n");
        try atomicWrite(self.allocator, self.io, self.states_path, out.items);
    }
};

fn stateBaseDir(allocator: std.mem.Allocator) ![]u8 {
    if (try util.envOwned(allocator, "XDG_STATE_HOME")) |xdg| {
        defer allocator.free(xdg);
        return std.fmt.allocPrint(allocator, "{s}/diffo", .{xdg});
    }
    const home = (try util.envOwned(allocator, "HOME")) orelse try util.dupe(allocator, ".");
    defer allocator.free(home);
    return std.fmt.allocPrint(allocator, "{s}/.local/state/diffo", .{home});
}

fn loadComments(allocator: std.mem.Allocator, io: std.Io, path: []const u8) StoreError![]Comment {
    const content = std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(100 * 1024 * 1024)) catch |err| switch (err) {
        error.FileNotFound => return allocator.alloc(Comment, 0),
        else => return error.StorageCorrupted,
    };
    defer allocator.free(content);
    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{}) catch return error.StorageCorrupted;
    defer parsed.deinit();
    const root = parsed.value;
    if (root != .object) return error.StorageCorrupted;
    const comments_value = root.object.get("comments") orelse return allocator.alloc(Comment, 0);
    if (comments_value != .array) return error.StorageCorrupted;
    var comments: std.ArrayList(Comment) = .empty;
    errdefer {
        for (comments.items) |*comment| comment.deinit(allocator);
        comments.deinit(allocator);
    }
    for (comments_value.array.items) |item| {
        if (item != .object) continue;
        try comments.append(allocator, try commentFromJson(allocator, item));
    }
    return comments.toOwnedSlice(allocator);
}

fn loadStates(allocator: std.mem.Allocator, io: std.Io, path: []const u8) StoreError![]ReviewState {
    const content = std.Io.Dir.cwd().readFileAlloc(io, path, allocator, .limited(100 * 1024 * 1024)) catch |err| switch (err) {
        error.FileNotFound => return allocator.alloc(ReviewState, 0),
        else => return error.StorageCorrupted,
    };
    defer allocator.free(content);
    var parsed = std.json.parseFromSlice(std.json.Value, allocator, content, .{}) catch return error.StorageCorrupted;
    defer parsed.deinit();
    const root = parsed.value;
    if (root != .object) return error.StorageCorrupted;
    const states_value = root.object.get("states") orelse return allocator.alloc(ReviewState, 0);
    if (states_value != .array) return error.StorageCorrupted;
    var states: std.ArrayList(ReviewState) = .empty;
    errdefer {
        for (states.items) |*state| state.deinit(allocator);
        states.deinit(allocator);
    }
    for (states_value.array.items) |item| {
        if (item != .object) continue;
        try states.append(allocator, .{
            .repository_id = try jsonString(allocator, item, "repository_id", ""),
            .review_target_id = try jsonString(allocator, item, "review_target_id", ""),
            .file_path = try jsonString(allocator, item, "file_path", ""),
            .status = try jsonString(allocator, item, "status", "unreviewed"),
            .patch_fingerprint = try jsonString(allocator, item, "patch_fingerprint", ""),
            .updated_at = try jsonString(allocator, item, "updated_at", ""),
        });
    }
    return states.toOwnedSlice(allocator);
}

fn commentFromJson(allocator: std.mem.Allocator, item: std.json.Value) StoreError!Comment {
    const maybe_anchor = item.object.get("anchor");
    const stable_line_id = blk: {
        const anchor = maybe_anchor orelse break :blk "";
        if (anchor != .object) break :blk "";
        const ids = anchor.object.get("stable_line_ids") orelse break :blk "";
        if (ids != .array or ids.array.items.len == 0 or ids.array.items[0] != .string) break :blk "";
        break :blk ids.array.items[0].string;
    };
    const match_label = if (maybe_anchor) |anchor| (jsonStringView(anchor, "match_status") orelse "exact") else "exact";
    const anchor_for_values = maybe_anchor orelse item;
    return .{
        .comment_id = try jsonString(allocator, item, "comment_id", ""),
        .repository_id = try jsonString(allocator, item, "repository_id", ""),
        .review_target_id = try jsonString(allocator, item, "review_target_id", ""),
        .file_path = try jsonString(allocator, item, "file_path", ""),
        .side = try jsonString(allocator, anchor_for_values, "side", "new"),
        .start_line = jsonInt(anchor_for_values, "start_line", 0),
        .end_line = jsonInt(anchor_for_values, "end_line", 0),
        .stable_line_id = try util.dupe(allocator, stable_line_id),
        .hunk_header = try jsonString(allocator, anchor_for_values, "hunk_header", ""),
        .context_before = try util.dupe(allocator, ""),
        .context_after = try util.dupe(allocator, ""),
        .patch_fingerprint = try jsonString(allocator, anchor_for_values, "patch_fingerprint", ""),
        .match_status = parseMatchStatus(match_label),
        .body = try jsonString(allocator, item, "body", ""),
        .author = try jsonString(allocator, item, "author", ""),
        .created_at = try jsonString(allocator, item, "created_at", ""),
        .updated_at = try jsonString(allocator, item, "updated_at", ""),
    };
}

fn cloneComment(allocator: std.mem.Allocator, comment: Comment) !Comment {
    return .{
        .comment_id = try util.dupe(allocator, comment.comment_id),
        .repository_id = try util.dupe(allocator, comment.repository_id),
        .review_target_id = try util.dupe(allocator, comment.review_target_id),
        .file_path = try util.dupe(allocator, comment.file_path),
        .side = try util.dupe(allocator, comment.side),
        .start_line = comment.start_line,
        .end_line = comment.end_line,
        .stable_line_id = try util.dupe(allocator, comment.stable_line_id),
        .hunk_header = try util.dupe(allocator, comment.hunk_header),
        .context_before = try util.dupe(allocator, comment.context_before),
        .context_after = try util.dupe(allocator, comment.context_after),
        .patch_fingerprint = try util.dupe(allocator, comment.patch_fingerprint),
        .match_status = comment.match_status,
        .body = try util.dupe(allocator, comment.body),
        .author = try util.dupe(allocator, comment.author),
        .created_at = try util.dupe(allocator, comment.created_at),
        .updated_at = try util.dupe(allocator, comment.updated_at),
    };
}

fn parseMatchStatus(label: []const u8) MatchStatus {
    if (util.eql(label, "relocated")) return .relocated;
    if (util.eql(label, "stale")) return .stale;
    if (util.eql(label, "missing")) return .missing;
    return .exact;
}

fn jsonString(allocator: std.mem.Allocator, item: std.json.Value, key: []const u8, default: []const u8) ![]u8 {
    if (jsonStringView(item, key)) |value| return util.dupe(allocator, value);
    return util.dupe(allocator, default);
}

fn jsonStringView(item: std.json.Value, key: []const u8) ?[]const u8 {
    if (item != .object) return null;
    const value = item.object.get(key) orelse return null;
    if (value != .string) return null;
    return value.string;
}

fn jsonInt(item: std.json.Value, key: []const u8, default: u32) u32 {
    if (item != .object) return default;
    const value = item.object.get(key) orelse return default;
    return switch (value) {
        .integer => |i| if (i < 0) default else @as(u32, @intCast(i)),
        .float => |f| if (f < 0) default else @as(u32, @intFromFloat(f)),
        else => default,
    };
}

fn writeJsonField(out: *std.ArrayList(u8), allocator: std.mem.Allocator, key: []const u8, value: []const u8, comma: bool) !void {
    try out.appendSlice(allocator, "        \"");
    try out.appendSlice(allocator, key);
    try out.appendSlice(allocator, "\": ");
    try util.writeJsonString(out, allocator, value);
    if (comma) try out.append(allocator, ',');
    try out.append(allocator, '\n');
}

fn writeIntField(out: *std.ArrayList(u8), allocator: std.mem.Allocator, key: []const u8, value: u32, comma: bool) !void {
    const rendered = try std.fmt.allocPrint(allocator, "        \"{s}\": {d}", .{ key, value });
    defer allocator.free(rendered);
    try out.appendSlice(allocator, rendered);
    if (comma) try out.append(allocator, ',');
    try out.append(allocator, '\n');
}

fn atomicWrite(allocator: std.mem.Allocator, io: std.Io, path: []const u8, bytes: []const u8) StoreError!void {
    const now_ms = @divTrunc(std.Io.Clock.real.now(io).nanoseconds, std.time.ns_per_ms);
    const tmp = try std.fmt.allocPrint(allocator, "{s}.tmp.{d}", .{ path, now_ms });
    defer allocator.free(tmp);
    var file = std.Io.Dir.createFileAbsolute(io, tmp, .{ .truncate = true }) catch return error.StorageWriteFailed;
    defer file.close(io);
    file.writeStreamingAll(io, bytes) catch return error.StorageWriteFailed;
    file.sync(io) catch return error.StorageWriteFailed;
    std.Io.Dir.renameAbsolute(tmp, path, io) catch return error.StorageWriteFailed;
}

test "state invalidates by patch fingerprint" {
    const allocator = std.testing.allocator;
    var tmp = std.testing.tmpDir(.{});
    defer tmp.cleanup();
    const tmp_path_z = try tmp.dir.realPathFileAlloc(std.testing.io, ".", allocator);
    const tmp_path: []const u8 = tmp_path_z;
    defer allocator.free(tmp_path_z);

    var store = Store{
        .allocator = allocator,
        .io = std.testing.io,
        .repo_dir = try util.dupe(allocator, tmp_path),
        .comments_path = try std.fmt.allocPrint(allocator, "{s}/comments.json", .{tmp_path}),
        .states_path = try std.fmt.allocPrint(allocator, "{s}/review-states.json", .{tmp_path}),
        .comments = try allocator.alloc(Comment, 0),
        .states = try allocator.alloc(ReviewState, 0),
    };
    defer store.deinit();
    var file = diff.DiffFile{
        .path = try util.dupe(allocator, "src/main.zig"),
        .old_path = null,
        .status = .modified,
        .source = .explicit,
        .language = null,
        .is_binary = false,
        .hunks = try allocator.alloc(diff.DiffHunk, 0),
        .patch_fingerprint = try util.dupe(allocator, "sha256:a"),
        .patch_text = try util.dupe(allocator, ""),
    };
    defer file.deinit(allocator);
    try store.setReviewed("repo_test", "target_test", file, true);
    try std.testing.expect(store.isReviewed("src/main.zig", "sha256:a", "target_test"));
    try std.testing.expect(!store.isReviewed("src/main.zig", "sha256:b", "target_test"));
}
