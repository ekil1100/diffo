const std = @import("std");
const diff = @import("diff.zig");
const git = @import("git.zig");
const store_mod = @import("store.zig");
const theme = @import("theme.zig");
const tui = @import("tui.zig");
const util = @import("util.zig");

pub const CliError = anyerror;

pub fn run(allocator: std.mem.Allocator, io: std.Io, argv: []const []const u8) CliError!void {
    var debug_git = false;
    var args_list: std.ArrayList([]const u8) = .empty;
    defer args_list.deinit(allocator);
    for (argv) |arg| {
        if (util.eql(arg, "--debug-git")) debug_git = true else try args_list.append(allocator, arg);
    }
    const args = args_list.items;
    if (args.len > 0 and (util.eql(args[0], "--help") or util.eql(args[0], "-h"))) {
        try writeAll(io, helpText);
        return;
    }
    if (args.len > 0 and util.eql(args[0], "comments")) return commentsCommand(allocator, io, args[1..], debug_git);
    if (args.len > 0 and util.eql(args[0], "review")) return reviewCommand(allocator, io, args[1..], debug_git);
    if (args.len > 0 and util.eql(args[0], "themes")) return themesCommand(allocator, io, args[1..]);

    var target = try diff.makeReviewTarget(allocator, args);
    errdefer target.deinit(allocator);
    var repo = try git.discoverRepository(allocator, io, debug_git);
    errdefer repo.deinit(allocator);
    var snapshot = try git.loadSnapshot(allocator, io, repo, target, debug_git);
    defer snapshot.deinit(allocator);
    var store = try store_mod.Store.init(allocator, io, snapshot.repository.repo_id);
    defer store.deinit();
    const author = try defaultAuthor(allocator);
    defer allocator.free(author);
    try tui.run(allocator, io, &snapshot, &store, author);
}

fn commentsCommand(allocator: std.mem.Allocator, io: std.Io, args: []const []const u8, debug_git: bool) CliError!void {
    if (args.len == 0 or (!util.eql(args[0], "list") and !util.eql(args[0], "get") and !util.eql(args[0], "add") and !util.eql(args[0], "clean") and !util.eql(args[0], "cleanup"))) return error.InvalidArguments;
    if (util.eql(args[0], "list")) {
        var file_filter: ?[]const u8 = null;
        var json = false;
        var i: usize = 1;
        while (i < args.len) : (i += 1) {
            if (util.eql(args[i], "--json")) json = true else if (util.eql(args[i], "--file")) {
                i += 1;
                if (i >= args.len) return error.InvalidArguments;
                file_filter = args[i];
            } else return error.InvalidArguments;
        }
        var context = try loadDefaultContext(allocator, io, debug_git);
        defer context.deinit();
        context.store.refreshMatchStatus(context.snapshot);
        if (json) try printCommentsJson(allocator, io, context.snapshot, context.store, file_filter) else try printCommentsText(allocator, io, context.store, file_filter);
        return;
    }
    if (util.eql(args[0], "get")) {
        if (args.len < 2) return error.InvalidArguments;
        const id = args[1];
        const json = hasFlag(args[2..], "--json");
        var context = try loadDefaultContext(allocator, io, debug_git);
        defer context.deinit();
        context.store.refreshMatchStatus(context.snapshot);
        for (context.store.comments) |comment| {
            if (util.eql(comment.comment_id, id)) {
                if (json) try printOneCommentJson(allocator, io, comment) else try printOneCommentText(allocator, io, comment);
                return;
            }
        }
        return error.InvalidArguments;
    }
    if (util.eql(args[0], "add")) {
        var file_path: ?[]const u8 = null;
        var line_no: ?u32 = null;
        var end_line: u32 = 0;
        var body: ?[]const u8 = null;
        var i: usize = 1;
        while (i < args.len) : (i += 1) {
            if (util.eql(args[i], "--file")) {
                i += 1;
                if (i >= args.len) return error.InvalidArguments;
                file_path = args[i];
            } else if (util.eql(args[i], "--line")) {
                i += 1;
                if (i >= args.len) return error.InvalidArguments;
                line_no = std.fmt.parseInt(u32, args[i], 10) catch return error.InvalidArguments;
            } else if (util.eql(args[i], "--end")) {
                i += 1;
                if (i >= args.len) return error.InvalidArguments;
                end_line = std.fmt.parseInt(u32, args[i], 10) catch return error.InvalidArguments;
            } else if (util.eql(args[i], "--body")) {
                i += 1;
                if (i >= args.len) return error.InvalidArguments;
                body = args[i];
            } else return error.InvalidArguments;
        }
        if (file_path == null or line_no == null or body == null) return error.InvalidArguments;
        var context = try loadDefaultContext(allocator, io, debug_git);
        defer context.deinit();
        const author = try defaultAuthor(allocator);
        defer allocator.free(author);
        const file = findFile(context.snapshot, file_path.?) orelse return error.InvalidArguments;
        const line = findDiffLineByNumber(file.*, line_no.?) orelse return error.InvalidArguments;
        var comment = try context.store.addComment(context.snapshot.repository.repo_id, context.snapshot.review_target.target_id, file.*, line.line, line.hunk_header, end_line, body.?, author);
        defer comment.deinit(allocator);
        try printOneCommentText(allocator, io, comment);
        return;
    }
    if (util.eql(args[0], "clean") or util.eql(args[0], "cleanup")) {
        var file_filter: ?[]const u8 = null;
        var json = false;
        var dry_run = false;
        var all = false;
        var i: usize = 1;
        while (i < args.len) : (i += 1) {
            if (util.eql(args[i], "--json")) {
                json = true;
            } else if (util.eql(args[i], "--dry-run")) {
                dry_run = true;
            } else if (util.eql(args[i], "--all")) {
                all = true;
            } else if (util.eql(args[i], "--file")) {
                i += 1;
                if (i >= args.len) return error.InvalidArguments;
                file_filter = args[i];
            } else return error.InvalidArguments;
        }
        var context = try loadDefaultContext(allocator, io, debug_git);
        defer context.deinit();
        context.store.refreshMatchStatus(context.snapshot);
        const removed_count = if (all)
            if (dry_run)
                context.store.allCommentCount(file_filter)
            else
                try context.store.removeAllComments(file_filter)
        else if (dry_run)
            context.store.outdatedCommentCount(context.snapshot.review_target.target_id, file_filter)
        else
            try context.store.removeOutdatedComments(context.snapshot.review_target.target_id, file_filter);
        if (json) {
            try printCleanCommentsJson(allocator, io, context.snapshot, removed_count, dry_run, all);
        } else {
            try printCleanCommentsText(allocator, io, removed_count, dry_run, all);
        }
        return;
    }
}

fn reviewCommand(allocator: std.mem.Allocator, io: std.Io, args: []const []const u8, debug_git: bool) CliError!void {
    if (args.len == 0) return error.InvalidArguments;
    if (util.eql(args[0], "status")) {
        var file_filter: ?[]const u8 = null;
        var json = false;
        var i: usize = 1;
        while (i < args.len) : (i += 1) {
            if (util.eql(args[i], "--json")) json = true else if (util.eql(args[i], "--file")) {
                i += 1;
                if (i >= args.len) return error.InvalidArguments;
                file_filter = args[i];
            } else return error.InvalidArguments;
        }
        var context = try loadDefaultContext(allocator, io, debug_git);
        defer context.deinit();
        if (json) try printReviewJson(allocator, io, context.snapshot, context.store, file_filter) else try printReviewText(allocator, io, context.snapshot, context.store, file_filter);
        return;
    }
    if (util.eql(args[0], "mark")) {
        var file_path: ?[]const u8 = null;
        var reviewed = true;
        var i: usize = 1;
        while (i < args.len) : (i += 1) {
            if (util.eql(args[i], "--file")) {
                i += 1;
                if (i >= args.len) return error.InvalidArguments;
                file_path = args[i];
            } else if (util.eql(args[i], "--unreviewed")) reviewed = false else if (util.eql(args[i], "--reviewed")) reviewed = true else return error.InvalidArguments;
        }
        if (file_path == null) return error.InvalidArguments;
        var context = try loadDefaultContext(allocator, io, debug_git);
        defer context.deinit();
        const file = findFile(context.snapshot, file_path.?) orelse return error.InvalidArguments;
        try context.store.setReviewed(context.snapshot.repository.repo_id, context.snapshot.review_target.target_id, file.*, reviewed);
        try printReviewText(allocator, io, context.snapshot, context.store, file_path);
        return;
    }
    return error.InvalidArguments;
}

fn themesCommand(allocator: std.mem.Allocator, io: std.Io, args: []const []const u8) CliError!void {
    if (args.len == 0) return error.InvalidArguments;
    if (util.eql(args[0], "list")) {
        const text = try theme.listBuiltins(allocator);
        defer allocator.free(text);
        try writeAll(io, text);
        return;
    }
    if (util.eql(args[0], "validate")) {
        if (args.len != 2) return error.InvalidArguments;
        const result = try theme.validateBaseThemeFile(allocator, io, args[1]);
        defer allocator.free(result);
        try writeAll(io, result);
        try writeAll(io, "\n");
        return;
    }
    return error.InvalidArguments;
}

const Context = struct {
    snapshot: diff.DiffSnapshot,
    store: store_mod.Store,

    fn deinit(self: *Context) void {
        self.store.deinit();
        self.snapshot.deinit(self.store.allocator);
    }
};

fn loadDefaultContext(allocator: std.mem.Allocator, io: std.Io, debug_git: bool) CliError!Context {
    var target = try diff.makeReviewTarget(allocator, &.{});
    errdefer target.deinit(allocator);
    var repo = try git.discoverRepository(allocator, io, debug_git);
    errdefer repo.deinit(allocator);
    var snapshot = try git.loadSnapshot(allocator, io, repo, target, debug_git);
    errdefer snapshot.deinit(allocator);
    const store = try store_mod.Store.init(allocator, io, snapshot.repository.repo_id);
    return .{ .snapshot = snapshot, .store = store };
}

fn printCommentsText(allocator: std.mem.Allocator, io: std.Io, store: store_mod.Store, file_filter: ?[]const u8) !void {
    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    for (store.comments) |comment| {
        if (file_filter) |file| if (!util.eql(comment.file_path, file)) continue;
        const line = try std.fmt.allocPrint(allocator, "{s} {s}:{d}-{d} [{s}] {s}\n{s}\n", .{ comment.comment_id, comment.file_path, comment.start_line, comment.end_line, comment.match_status.label(), comment.author, comment.body });
        defer allocator.free(line);
        try out.appendSlice(allocator, line);
    }
    if (out.items.len == 0) try out.appendSlice(allocator, "no comments\n");
    try writeAll(io, out.items);
}

fn printOneCommentText(allocator: std.mem.Allocator, io: std.Io, comment: store_mod.Comment) !void {
    const line = try std.fmt.allocPrint(allocator, "{s} {s}:{d}-{d} [{s}] {s}\n{s}\n", .{ comment.comment_id, comment.file_path, comment.start_line, comment.end_line, comment.match_status.label(), comment.author, comment.body });
    defer allocator.free(line);
    try writeAll(io, line);
}

fn printCommentsJson(allocator: std.mem.Allocator, io: std.Io, snapshot: diff.DiffSnapshot, store: store_mod.Store, file_filter: ?[]const u8) !void {
    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    try out.appendSlice(allocator, "{\n  \"schema_version\": 1,\n  \"repository_id\": ");
    try util.writeJsonString(&out, allocator, snapshot.repository.repo_id);
    try out.appendSlice(allocator, ",\n  \"review_target_id\": ");
    try util.writeJsonString(&out, allocator, snapshot.review_target.target_id);
    try out.appendSlice(allocator, ",\n  \"comments\": [\n");
    var emitted: usize = 0;
    for (store.comments) |comment| {
        if (file_filter) |file| if (!util.eql(comment.file_path, file)) continue;
        if (emitted > 0) try out.appendSlice(allocator, ",\n");
        try appendCommentJson(allocator, &out, comment, "    ");
        emitted += 1;
    }
    try out.appendSlice(allocator, "\n  ]\n}\n");
    try writeAll(io, out.items);
}

fn printOneCommentJson(allocator: std.mem.Allocator, io: std.Io, comment: store_mod.Comment) !void {
    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    try appendCommentJson(allocator, &out, comment, "");
    try out.append(allocator, '\n');
    try writeAll(io, out.items);
}

fn printCleanCommentsText(allocator: std.mem.Allocator, io: std.Io, removed_count: usize, dry_run: bool, all: bool) !void {
    const verb = if (dry_run) "would remove" else "removed";
    const subject = if (all) "comments" else "outdated comments";
    const line = try std.fmt.allocPrint(allocator, "{s} {d} {s}\n", .{ verb, removed_count, subject });
    defer allocator.free(line);
    try writeAll(io, line);
}

fn printCleanCommentsJson(
    allocator: std.mem.Allocator,
    io: std.Io,
    snapshot: diff.DiffSnapshot,
    removed_count: usize,
    dry_run: bool,
    all: bool,
) !void {
    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    try out.appendSlice(allocator, "{\n  \"schema_version\": 1,\n  \"repository_id\": ");
    try util.writeJsonString(&out, allocator, snapshot.repository.repo_id);
    try out.appendSlice(allocator, ",\n  \"review_target_id\": ");
    try util.writeJsonString(&out, allocator, snapshot.review_target.target_id);
    try out.appendSlice(allocator, ",\n  \"removed_count\": ");
    const count_text = try std.fmt.allocPrint(allocator, "{d}", .{removed_count});
    defer allocator.free(count_text);
    try out.appendSlice(allocator, count_text);
    try out.appendSlice(allocator, ",\n  \"dry_run\": ");
    try out.appendSlice(allocator, if (dry_run) "true" else "false");
    try out.appendSlice(allocator, ",\n  \"all\": ");
    try out.appendSlice(allocator, if (all) "true" else "false");
    try out.appendSlice(allocator, "\n}\n");
    try writeAll(io, out.items);
}

fn appendCommentJson(allocator: std.mem.Allocator, out: *std.ArrayList(u8), comment: store_mod.Comment, indent: []const u8) !void {
    try out.appendSlice(allocator, indent);
    try out.appendSlice(allocator, "{\n");
    try jsonField(allocator, out, indent, "comment_id", comment.comment_id, true);
    try jsonField(allocator, out, indent, "file_path", comment.file_path, true);
    try intField(allocator, out, indent, "start_line", comment.start_line, true);
    try intField(allocator, out, indent, "end_line", comment.end_line, true);
    try jsonField(allocator, out, indent, "side", comment.side, true);
    try jsonField(allocator, out, indent, "body", comment.body, true);
    try jsonField(allocator, out, indent, "author", comment.author, true);
    try jsonField(allocator, out, indent, "match_status", comment.match_status.label(), true);
    try out.appendSlice(allocator, indent);
    try out.appendSlice(allocator, "  \"anchor\": {\n");
    try out.appendSlice(allocator, indent);
    try out.appendSlice(allocator, "    \"hunk_header\": ");
    try util.writeJsonString(out, allocator, comment.hunk_header);
    try out.appendSlice(allocator, ",\n");
    try out.appendSlice(allocator, indent);
    try out.appendSlice(allocator, "    \"patch_fingerprint\": ");
    try util.writeJsonString(out, allocator, comment.patch_fingerprint);
    try out.appendSlice(allocator, ",\n");
    try out.appendSlice(allocator, indent);
    try out.appendSlice(allocator, "    \"stable_line_ids\": [");
    try util.writeJsonString(out, allocator, comment.stable_line_id);
    try out.appendSlice(allocator, "]\n");
    try out.appendSlice(allocator, indent);
    try out.appendSlice(allocator, "  },\n");
    try jsonField(allocator, out, indent, "review_target_id", comment.review_target_id, false);
    try out.appendSlice(allocator, indent);
    try out.append(allocator, '}');
}

fn printReviewText(allocator: std.mem.Allocator, io: std.Io, snapshot: diff.DiffSnapshot, store: store_mod.Store, file_filter: ?[]const u8) !void {
    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    for (snapshot.files) |file| {
        if (file_filter) |filter| if (!util.eql(file.path, filter)) continue;
        const status = store.statusForFile(file.path, file.patch_fingerprint, snapshot.review_target.target_id);
        const line = try std.fmt.allocPrint(allocator, "{s} {s} comments={d} fingerprint={s}\n", .{ status, file.path, store.commentCount(file.path), file.patch_fingerprint });
        defer allocator.free(line);
        try out.appendSlice(allocator, line);
    }
    if (out.items.len == 0) try out.appendSlice(allocator, "no changed files\n");
    try writeAll(io, out.items);
}

fn printReviewJson(allocator: std.mem.Allocator, io: std.Io, snapshot: diff.DiffSnapshot, store: store_mod.Store, file_filter: ?[]const u8) !void {
    var out: std.ArrayList(u8) = .empty;
    defer out.deinit(allocator);
    try out.appendSlice(allocator, "{\n  \"schema_version\": 1,\n  \"repository_id\": ");
    try util.writeJsonString(&out, allocator, snapshot.repository.repo_id);
    try out.appendSlice(allocator, ",\n  \"review_target_id\": ");
    try util.writeJsonString(&out, allocator, snapshot.review_target.target_id);
    try out.appendSlice(allocator, ",\n  \"files\": [\n");
    var emitted: usize = 0;
    for (snapshot.files) |file| {
        if (file_filter) |filter| if (!util.eql(file.path, filter)) continue;
        if (emitted > 0) try out.appendSlice(allocator, ",\n");
        try out.appendSlice(allocator, "    {\n      \"file_path\": ");
        try util.writeJsonString(&out, allocator, file.path);
        try out.appendSlice(allocator, ",\n      \"status\": ");
        try util.writeJsonString(&out, allocator, store.statusForFile(file.path, file.patch_fingerprint, snapshot.review_target.target_id));
        try out.appendSlice(allocator, ",\n      \"patch_fingerprint\": ");
        try util.writeJsonString(&out, allocator, file.patch_fingerprint);
        try out.appendSlice(allocator, ",\n      \"comment_count\": ");
        const rendered = try std.fmt.allocPrint(allocator, "{d}\n    }}", .{store.commentCount(file.path)});
        defer allocator.free(rendered);
        try out.appendSlice(allocator, rendered);
        emitted += 1;
    }
    try out.appendSlice(allocator, "\n  ]\n}\n");
    try writeAll(io, out.items);
}

fn jsonField(allocator: std.mem.Allocator, out: *std.ArrayList(u8), indent: []const u8, key: []const u8, value: []const u8, comma: bool) !void {
    try out.appendSlice(allocator, indent);
    try out.appendSlice(allocator, "  \"");
    try out.appendSlice(allocator, key);
    try out.appendSlice(allocator, "\": ");
    try util.writeJsonString(out, allocator, value);
    if (comma) try out.append(allocator, ',');
    try out.append(allocator, '\n');
}

fn intField(allocator: std.mem.Allocator, out: *std.ArrayList(u8), indent: []const u8, key: []const u8, value: u32, comma: bool) !void {
    const line = try std.fmt.allocPrint(allocator, "{s}  \"{s}\": {d}{s}\n", .{ indent, key, value, if (comma) "," else "" });
    defer allocator.free(line);
    try out.appendSlice(allocator, line);
}

fn hasFlag(args: []const []const u8, flag: []const u8) bool {
    for (args) |arg| if (util.eql(arg, flag)) return true;
    return false;
}

fn defaultAuthor(allocator: std.mem.Allocator) ![]u8 {
    if (try util.envOwned(allocator, "GIT_AUTHOR_NAME")) |author| return author;
    if (try util.envOwned(allocator, "USER")) |user| return user;
    return util.dupe(allocator, "local");
}

fn findFile(snapshot: diff.DiffSnapshot, path: []const u8) ?*const diff.DiffFile {
    for (snapshot.files) |*file| if (util.eql(file.path, path)) return file;
    return null;
}

const LineMatch = struct {
    line: *const diff.DiffLine,
    hunk_header: []const u8,
};

fn findDiffLineByNumber(file: diff.DiffFile, line_no: u32) ?LineMatch {
    for (file.hunks) |hunk| {
        for (hunk.lines) |*line| {
            if (line.new_lineno == line_no or line.old_lineno == line_no) return .{ .line = line, .hunk_header = hunk.header };
        }
    }
    return null;
}

fn writeAll(io: std.Io, bytes: []const u8) !void {
    try std.Io.File.stdout().writeStreamingAll(io, bytes);
}

const helpText =
    \\diffo - terminal Git diff review
    \\
    \\Usage:
    \\  diffo [git-diff-args]
    \\  diffo comments list [--file <path>] [--json]
    \\  diffo comments get <comment-id> [--json]
    \\  diffo comments add --file <path> --line <n> [--end <n>] --body <text>
    \\  diffo comments clean [--all] [--file <path>] [--dry-run] [--json]
    \\  diffo review status [--file <path>] [--json]
    \\  diffo review mark --file <path> [--reviewed|--unreviewed]
    \\  diffo themes list
    \\  diffo themes validate <file>
    \\
    \\Interactive keys:
    \\  j/k line, J/K file, n/N change, C unfold/fold mode, z/Z folds, v stacked/split, r reviewed, c comment, V select, y copy, Esc clear selection, u unreviewed, ? help, q quit
    \\
;
