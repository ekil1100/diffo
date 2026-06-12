const std = @import("std");
const diff = @import("diff.zig");
const util = @import("util.zig");

const full_context_arg = "--unified=1000000";

pub const GitError = error{
    NotGitRepository,
    GitCommandFailed,
    DiffTooLarge,
} || std.mem.Allocator.Error || diff.ParseError;

pub const FileSide = enum {
    old,
    new,
};

pub const GitRunner = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    repo_root: []const u8,
    debug: bool = false,

    pub const Result = struct {
        stdout: []u8,
        stderr: []u8,

        pub fn deinit(self: *Result, allocator: std.mem.Allocator) void {
            allocator.free(self.stdout);
            allocator.free(self.stderr);
        }
    };

    pub fn run(self: GitRunner, args: []const []const u8) GitError!Result {
        var argv: std.ArrayList([]const u8) = .empty;
        defer argv.deinit(self.allocator);
        try argv.append(self.allocator, "git");
        try argv.append(self.allocator, "-C");
        try argv.append(self.allocator, self.repo_root);
        for (args) |arg| try argv.append(self.allocator, arg);

        const result = std.process.run(self.allocator, self.io, .{
            .argv = argv.items,
            .stdout_limit = .limited(200 * 1024 * 1024),
            .stderr_limit = .limited(20 * 1024 * 1024),
        }) catch |err| switch (err) {
            // Distinguish an over-200MB diff from a real git failure so callers can report it.
            error.StreamTooLong => return error.DiffTooLarge,
            else => return error.GitCommandFailed,
        };

        switch (result.term) {
            .exited => |code| if (code != 0) {
                if (self.debug) std.Io.File.stderr().writeStreamingAll(self.io, result.stderr) catch {};
                self.allocator.free(result.stdout);
                self.allocator.free(result.stderr);
                return error.GitCommandFailed;
            },
            else => {
                self.allocator.free(result.stdout);
                self.allocator.free(result.stderr);
                return error.GitCommandFailed;
            },
        }

        return .{ .stdout = result.stdout, .stderr = result.stderr };
    }
};

pub fn discoverRepository(allocator: std.mem.Allocator, io: std.Io, debug: bool) GitError!diff.Repository {
    const root = try runGitNoRepo(allocator, io, &.{ "rev-parse", "--show-toplevel" }, debug);
    defer allocator.free(root.stderr);
    const trimmed_root = std.mem.trim(u8, root.stdout, "\r\n");
    if (trimmed_root.len == 0) {
        allocator.free(root.stdout);
        return error.NotGitRepository;
    }
    const root_path = try util.dupe(allocator, trimmed_root);
    allocator.free(root.stdout);
    errdefer allocator.free(root_path);

    const runner = GitRunner{ .allocator = allocator, .io = io, .repo_root = root_path, .debug = debug };
    var branch_result = runner.run(&.{ "rev-parse", "--abbrev-ref", "HEAD" }) catch |err| switch (err) {
        error.GitCommandFailed => GitRunner.Result{ .stdout = try util.dupe(allocator, "HEAD\n"), .stderr = try util.dupe(allocator, "") },
        else => return err,
    };
    defer branch_result.deinit(allocator);
    const branch = try util.dupe(allocator, std.mem.trim(u8, branch_result.stdout, "\r\n"));
    errdefer allocator.free(branch);

    const real_root_z = std.Io.Dir.realPathFileAbsoluteAlloc(io, root_path, allocator) catch try allocator.dupeZ(u8, root_path);
    const real_root: []const u8 = real_root_z;
    defer allocator.free(real_root_z);
    const repo_hash = try util.hashHex(allocator, real_root);
    defer allocator.free(repo_hash);
    const repo_id = try std.fmt.allocPrint(allocator, "repo_{s}", .{repo_hash[0..16]});

    return .{
        .root_path = root_path,
        .repo_id = repo_id,
        .current_branch = branch,
    };
}

fn runGitNoRepo(allocator: std.mem.Allocator, io: std.Io, args: []const []const u8, debug: bool) GitError!GitRunner.Result {
    var argv: std.ArrayList([]const u8) = .empty;
    defer argv.deinit(allocator);
    try argv.append(allocator, "git");
    for (args) |arg| try argv.append(allocator, arg);
    const result = std.process.run(allocator, io, .{
        .argv = argv.items,
        .stdout_limit = .limited(10 * 1024 * 1024),
        .stderr_limit = .limited(10 * 1024 * 1024),
    }) catch return error.NotGitRepository;
    switch (result.term) {
        .exited => |code| if (code != 0) {
            if (debug) std.Io.File.stderr().writeStreamingAll(io, result.stderr) catch {};
            allocator.free(result.stdout);
            allocator.free(result.stderr);
            return error.NotGitRepository;
        },
        else => {
            allocator.free(result.stdout);
            allocator.free(result.stderr);
            return error.NotGitRepository;
        },
    }
    return .{ .stdout = result.stdout, .stderr = result.stderr };
}

pub fn loadSnapshot(allocator: std.mem.Allocator, io: std.Io, repo: diff.Repository, target: diff.ReviewTarget, debug: bool) GitError!diff.DiffSnapshot {
    var files: std.ArrayList(diff.DiffFile) = .empty;
    errdefer {
        for (files.items) |*file| file.deinit(allocator);
        files.deinit(allocator);
    }

    const runner = GitRunner{ .allocator = allocator, .io = io, .repo_root = repo.root_path, .debug = debug };
    if (target.kind == .working_tree) {
        try appendPatch(allocator, &files, runner, &.{ "diff", "--patch", "--find-renames", "--no-ext-diff", full_context_arg }, .unstaged);
        try appendPatch(allocator, &files, runner, &.{ "diff", "--cached", "--patch", "--find-renames", "--no-ext-diff", full_context_arg }, .staged);
        try appendUntrackedFiles(allocator, io, &files, runner);
    } else {
        var args: std.ArrayList([]const u8) = .empty;
        defer args.deinit(allocator);
        try args.appendSlice(allocator, &.{ "diff", "--patch", "--find-renames", "--no-ext-diff", full_context_arg });
        for (target.raw_args) |arg| try args.append(allocator, arg);
        try appendPatch(allocator, &files, runner, args.items, .explicit);
    }

    const files_slice = try files.toOwnedSlice(allocator);
    errdefer {
        for (files_slice) |*file| file.deinit(allocator);
        allocator.free(files_slice);
    }

    var hash_input: std.ArrayList(u8) = .empty;
    defer hash_input.deinit(allocator);
    try hash_input.appendSlice(allocator, repo.repo_id);
    try hash_input.appendSlice(allocator, target.target_id);
    for (files_slice) |file| {
        try hash_input.appendSlice(allocator, file.path);
        try hash_input.appendSlice(allocator, file.patch_fingerprint);
    }
    const snapshot_hash = try util.hashHex(allocator, hash_input.items);
    defer allocator.free(snapshot_hash);
    const snapshot_id = try std.fmt.allocPrint(allocator, "snap_{s}", .{snapshot_hash[0..16]});

    return .{
        .snapshot_id = snapshot_id,
        .repository = repo,
        .review_target = target,
        .files = files_slice,
    };
}

fn appendUntrackedFiles(
    allocator: std.mem.Allocator,
    io: std.Io,
    files: *std.ArrayList(diff.DiffFile),
    runner: GitRunner,
) GitError!void {
    var result = try runner.run(&.{ "ls-files", "--others", "--exclude-standard", "-z" });
    defer result.deinit(allocator);
    if (result.stdout.len == 0) return;

    var patches: std.ArrayList(u8) = .empty;
    defer patches.deinit(allocator);

    var paths = std.mem.splitScalar(u8, result.stdout, 0);
    while (paths.next()) |path| {
        if (path.len == 0) continue;
        try appendUntrackedPatch(allocator, io, &patches, runner.repo_root, path);
    }

    if (patches.items.len == 0) return;
    const parsed = try diff.parsePatch(allocator, patches.items, .untracked);
    defer allocator.free(parsed);
    for (parsed) |file| {
        try mergeOrAppendFile(allocator, files, file);
    }
}

fn appendUntrackedPatch(
    allocator: std.mem.Allocator,
    io: std.Io,
    patches: *std.ArrayList(u8),
    repo_root: []const u8,
    path: []const u8,
) GitError!void {
    const absolute_path = try std.fmt.allocPrint(allocator, "{s}/{s}", .{ repo_root, path });
    defer allocator.free(absolute_path);

    const max_untracked_text_bytes = 20 * 1024 * 1024;
    const content = std.Io.Dir.cwd().readFileAlloc(io, absolute_path, allocator, .limited(max_untracked_text_bytes)) catch |err| switch (err) {
        error.StreamTooLong => {
            try appendBinaryUntrackedPatch(allocator, patches, path);
            return;
        },
        error.OutOfMemory => return error.OutOfMemory,
        // A single unreadable untracked entry (broken symlink, no read permission, a directory
        // returned by ls-files, ...) must not abort the whole snapshot; skip just this file.
        else => return,
    };
    defer allocator.free(content);

    if (std.mem.indexOfScalar(u8, content, 0) != null) {
        try appendBinaryUntrackedPatch(allocator, patches, path);
        return;
    }

    const line_count = countPatchLines(content);
    const header = try std.fmt.allocPrint(allocator,
        \\diff --git a/{s} b/{s}
        \\new file mode 100644
        \\index 0000000..0000000
        \\--- /dev/null
        \\+++ b/{s}
        \\
    , .{ path, path, path });
    defer allocator.free(header);
    try patches.appendSlice(allocator, header);

    if (line_count > 0) {
        const hunk_header = try std.fmt.allocPrint(allocator, "@@ -0,0 +1,{d} @@\n", .{line_count});
        defer allocator.free(hunk_header);
        try patches.appendSlice(allocator, hunk_header);
        var lines = std.mem.splitScalar(u8, content, '\n');
        var emitted: usize = 0;
        while (lines.next()) |line| {
            if (emitted == line_count) break;
            try patches.append(allocator, '+');
            try patches.appendSlice(allocator, util.trimLine(line));
            try patches.append(allocator, '\n');
            emitted += 1;
        }
    }
}

fn appendBinaryUntrackedPatch(
    allocator: std.mem.Allocator,
    patches: *std.ArrayList(u8),
    path: []const u8,
) !void {
    const patch = try std.fmt.allocPrint(allocator,
        \\diff --git a/{s} b/{s}
        \\new file mode 100644
        \\index 0000000..0000000
        \\Binary files /dev/null and b/{s} differ
        \\
    , .{ path, path, path });
    defer allocator.free(patch);
    try patches.appendSlice(allocator, patch);
}

fn countPatchLines(content: []const u8) usize {
    if (content.len == 0) return 0;
    var count: usize = 1;
    for (content) |byte| {
        if (byte == '\n') count += 1;
    }
    if (content[content.len - 1] == '\n') count -= 1;
    return count;
}

fn appendPatch(
    allocator: std.mem.Allocator,
    files: *std.ArrayList(diff.DiffFile),
    runner: GitRunner,
    git_args: []const []const u8,
    source: diff.DiffSource,
) GitError!void {
    var result = try runner.run(git_args);
    defer result.deinit(allocator);
    if (std.mem.trim(u8, result.stdout, "\r\n").len == 0) return;
    const parsed = try diff.parsePatch(allocator, result.stdout, source);
    defer allocator.free(parsed);
    for (parsed) |file| {
        try mergeOrAppendFile(allocator, files, file);
    }
}

fn mergeOrAppendFile(allocator: std.mem.Allocator, files: *std.ArrayList(diff.DiffFile), incoming_file: diff.DiffFile) !void {
    var incoming = incoming_file;
    // This function takes ownership of `incoming`: free it if anything below fails before it
    // is merged into an existing file or appended to the list.
    errdefer incoming.deinit(allocator);

    for (files.items) |*existing| {
        if (util.eql(existing.path, incoming.path)) {
            // Build every merged buffer before mutating `existing` (commit-last), so an OOM
            // partway through neither half-updates `existing` nor leaks an intermediate.
            const merged_patch = try std.fmt.allocPrint(allocator, "{s}\n{s}", .{ existing.patch_text, incoming.patch_text });
            errdefer allocator.free(merged_patch);
            const merged_hash = try util.hashHex(allocator, merged_patch);
            const merged_fingerprint = std.fmt.allocPrint(allocator, "sha256:{s}", .{merged_hash}) catch |err| {
                allocator.free(merged_hash);
                return err;
            };
            allocator.free(merged_hash);
            errdefer allocator.free(merged_fingerprint);

            const merged_hunks = try allocator.alloc(diff.DiffHunk, existing.hunks.len + incoming.hunks.len);
            @memcpy(merged_hunks[0..existing.hunks.len], existing.hunks);
            @memcpy(merged_hunks[existing.hunks.len..], incoming.hunks);

            // Commit: release the superseded buffers and adopt the merged ones.
            allocator.free(existing.patch_text);
            allocator.free(existing.patch_fingerprint);
            allocator.free(existing.hunks);
            existing.patch_text = merged_patch;
            existing.patch_fingerprint = merged_fingerprint;
            existing.hunks = merged_hunks;
            if (existing.source != incoming.source) existing.source = .explicit;

            // incoming's hunk *elements* were moved into merged_hunks; free everything else we
            // own on incoming, including the now-moved-out hunks backing array (F16/F27). This
            // is the success path, so the errdefers above do not run.
            allocator.free(incoming.path);
            if (incoming.old_path) |old| allocator.free(old);
            if (incoming.language) |lang| allocator.free(lang);
            allocator.free(incoming.patch_fingerprint);
            allocator.free(incoming.patch_text);
            allocator.free(incoming.hunks);
            return;
        }
    }
    try files.append(allocator, incoming);
}

pub fn loadFileSide(
    allocator: std.mem.Allocator,
    io: std.Io,
    repo: diff.Repository,
    target: diff.ReviewTarget,
    file: diff.DiffFile,
    side: FileSide,
    debug: bool,
) GitError!?[]u8 {
    if (file.is_binary) return null;
    const runner = GitRunner{ .allocator = allocator, .io = io, .repo_root = repo.root_path, .debug = debug };
    if (target.kind != .working_tree) return loadExplicitFileSide(allocator, io, runner, repo.root_path, target, file, side);
    return switch (file.source) {
        .unstaged => switch (side) {
            .old => try readIndexBlob(allocator, runner, file.old_path orelse file.path),
            .new => try readWorktreeFile(allocator, io, repo.root_path, file.path),
        },
        .staged => switch (side) {
            .old => try readHeadBlob(allocator, runner, file.old_path orelse file.path),
            .new => try readIndexBlob(allocator, runner, file.path),
        },
        .untracked => switch (side) {
            .old => null,
            .new => try readWorktreeFile(allocator, io, repo.root_path, file.path),
        },
        .explicit => null,
    };
}

fn loadExplicitFileSide(
    allocator: std.mem.Allocator,
    io: std.Io,
    runner: GitRunner,
    repo_root: []const u8,
    target: diff.ReviewTarget,
    file: diff.DiffFile,
    side: FileSide,
) GitError!?[]u8 {
    if (target.kind == .cached) {
        return switch (side) {
            .old => try readHeadBlob(allocator, runner, file.old_path orelse file.path),
            .new => try readIndexBlob(allocator, runner, file.path),
        };
    }

    var revs: std.ArrayList([]const u8) = .empty;
    defer revs.deinit(allocator);
    try collectRevisionArgs(allocator, runner, &revs, target.raw_args);

    switch (target.kind) {
        .commit => {
            if (revs.items.len == 0) {
                return switch (side) {
                    .old => try readIndexBlob(allocator, runner, file.old_path orelse file.path),
                    .new => try readWorktreeFile(allocator, io, repo_root, file.path),
                };
            }
            if (revs.items.len == 1) {
                return switch (side) {
                    .old => try readBlobAtRevision(allocator, runner, revs.items[0], file.old_path orelse file.path),
                    .new => try readWorktreeFile(allocator, io, repo_root, file.path),
                };
            }
            if (revs.items.len >= 2) {
                return switch (side) {
                    .old => try readBlobAtRevision(allocator, runner, revs.items[0], file.old_path orelse file.path),
                    .new => try readBlobAtRevision(allocator, runner, revs.items[1], file.path),
                };
            }
        },
        .range => if (rangeRevisions(target.raw_args)) |range| {
            return switch (side) {
                .old => try readBlobAtRevision(allocator, runner, revisionOrHead(range.old), file.old_path orelse file.path),
                .new => try readBlobAtRevision(allocator, runner, revisionOrHead(range.new), file.path),
            };
        },
        .symmetric_range => if (rangeRevisions(target.raw_args)) |range| {
            switch (side) {
                .old => {
                    const base = try mergeBase(allocator, runner, revisionOrHead(range.old), revisionOrHead(range.new));
                    defer allocator.free(base);
                    return try readBlobAtRevision(allocator, runner, base, file.old_path orelse file.path);
                },
                .new => return try readBlobAtRevision(allocator, runner, revisionOrHead(range.new), file.path),
            }
        },
        else => {},
    }

    return null;
}

fn collectRevisionArgs(allocator: std.mem.Allocator, runner: GitRunner, revs: *std.ArrayList([]const u8), args: []const []const u8) !void {
    var before_pathspec = true;
    for (args) |arg| {
        if (util.eql(arg, "--")) {
            before_pathspec = false;
            continue;
        }
        if (!before_pathspec or util.startsWith(arg, "-")) continue;
        if (util.contains(arg, "..")) continue;
        if (!isRevision(runner, arg)) continue;
        try revs.append(allocator, arg);
    }
}

fn isRevision(runner: GitRunner, value: []const u8) bool {
    const spec = std.fmt.allocPrint(runner.allocator, "{s}^{{commit}}", .{value}) catch return false;
    defer runner.allocator.free(spec);

    var result = runner.run(&.{ "rev-parse", "--verify", "--quiet", spec }) catch return false;
    result.deinit(runner.allocator);
    return true;
}

const RangeRevisions = struct {
    old: []const u8,
    new: []const u8,
};

fn rangeRevisions(args: []const []const u8) ?RangeRevisions {
    for (args) |arg| {
        if (std.mem.indexOf(u8, arg, "...")) |i| {
            return .{ .old = arg[0..i], .new = arg[i + 3 ..] };
        }
        if (std.mem.indexOf(u8, arg, "..")) |i| {
            return .{ .old = arg[0..i], .new = arg[i + 2 ..] };
        }
    }
    return null;
}

fn revisionOrHead(rev: []const u8) []const u8 {
    return if (rev.len == 0) "HEAD" else rev;
}

fn mergeBase(allocator: std.mem.Allocator, runner: GitRunner, old: []const u8, new: []const u8) GitError![]u8 {
    const result = try runner.run(&.{ "merge-base", old, new });
    allocator.free(result.stderr);
    const trimmed = std.mem.trim(u8, result.stdout, "\r\n");
    const base = try util.dupe(allocator, trimmed);
    allocator.free(result.stdout);
    return base;
}

fn readHeadBlob(allocator: std.mem.Allocator, runner: GitRunner, path: []const u8) GitError!?[]u8 {
    const spec = try std.fmt.allocPrint(allocator, "HEAD:{s}", .{path});
    defer allocator.free(spec);
    return readGitBlob(allocator, runner, spec);
}

fn readBlobAtRevision(allocator: std.mem.Allocator, runner: GitRunner, rev: []const u8, path: []const u8) GitError!?[]u8 {
    if (rev.len == 0) return null;
    const spec = try std.fmt.allocPrint(allocator, "{s}:{s}", .{ rev, path });
    defer allocator.free(spec);
    return readGitBlob(allocator, runner, spec);
}

fn readIndexBlob(allocator: std.mem.Allocator, runner: GitRunner, path: []const u8) GitError!?[]u8 {
    const spec = try std.fmt.allocPrint(allocator, ":{s}", .{path});
    defer allocator.free(spec);
    return readGitBlob(allocator, runner, spec);
}

fn readGitBlob(allocator: std.mem.Allocator, runner: GitRunner, spec: []const u8) GitError!?[]u8 {
    const result = runner.run(&.{ "show", spec }) catch |err| switch (err) {
        error.GitCommandFailed => return null,
        else => return err,
    };
    allocator.free(result.stderr);
    return result.stdout;
}

fn readWorktreeFile(allocator: std.mem.Allocator, io: std.Io, repo_root: []const u8, path: []const u8) GitError!?[]u8 {
    const absolute = try std.fs.path.join(allocator, &.{ repo_root, path });
    defer allocator.free(absolute);
    const max_source_bytes = 4 * 1024 * 1024;
    return std.Io.Dir.cwd().readFileAlloc(io, absolute, allocator, .limited(max_source_bytes)) catch |err| switch (err) {
        error.StreamTooLong, error.FileNotFound, error.AccessDenied => null,
        else => error.GitCommandFailed,
    };
}

test "collect revision args skips pathspecs and options" {
    const allocator = std.testing.allocator;
    const runner = GitRunner{ .allocator = allocator, .io = std.testing.io, .repo_root = ".", .debug = false };
    var revs: std.ArrayList([]const u8) = .empty;
    defer revs.deinit(allocator);

    try collectRevisionArgs(allocator, runner, &revs, &.{ "--find-renames", "HEAD", "src/main.zig" });
    try std.testing.expectEqual(@as(usize, 1), revs.items.len);
    try std.testing.expectEqualStrings("HEAD", revs.items[0]);
}

test "range revisions split two-dot and three-dot specs" {
    const range = rangeRevisions(&.{"main..feature"}).?;
    try std.testing.expectEqualStrings("main", range.old);
    try std.testing.expectEqualStrings("feature", range.new);

    const symmetric = rangeRevisions(&.{"main...feature"}).?;
    try std.testing.expectEqualStrings("main", symmetric.old);
    try std.testing.expectEqualStrings("feature", symmetric.new);

    const omitted_old = rangeRevisions(&.{"..feature"}).?;
    try std.testing.expectEqualStrings("HEAD", revisionOrHead(omitted_old.old));
    try std.testing.expectEqualStrings("feature", revisionOrHead(omitted_old.new));

    const omitted_new = rangeRevisions(&.{"main..."}).?;
    try std.testing.expectEqualStrings("main", revisionOrHead(omitted_new.old));
    try std.testing.expectEqualStrings("HEAD", revisionOrHead(omitted_new.new));
}
