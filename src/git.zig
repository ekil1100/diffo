const std = @import("std");
const diff = @import("diff.zig");
const util = @import("util.zig");

const full_context_arg = "--unified=1000000";

pub const GitError = error{
    NotGitRepository,
    GitCommandFailed,
} || std.mem.Allocator.Error || diff.ParseError;

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
        }) catch return error.GitCommandFailed;

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
        else => return error.GitCommandFailed,
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

fn mergeOrAppendFile(allocator: std.mem.Allocator, files: *std.ArrayList(diff.DiffFile), incoming: diff.DiffFile) !void {
    for (files.items) |*existing| {
        if (util.eql(existing.path, incoming.path)) {
            const merged_patch = try std.fmt.allocPrint(allocator, "{s}\n{s}", .{ existing.patch_text, incoming.patch_text });
            const merged_hash = try util.hashHex(allocator, merged_patch);
            const merged_fingerprint = try std.fmt.allocPrint(allocator, "sha256:{s}", .{merged_hash});
            allocator.free(merged_hash);
            allocator.free(existing.patch_text);
            allocator.free(existing.patch_fingerprint);
            existing.patch_text = merged_patch;
            existing.patch_fingerprint = merged_fingerprint;
            if (existing.source != incoming.source) existing.source = .explicit;

            const old_len = existing.hunks.len;
            const merged_hunks = try allocator.alloc(diff.DiffHunk, existing.hunks.len + incoming.hunks.len);
            @memcpy(merged_hunks[0..existing.hunks.len], existing.hunks);
            @memcpy(merged_hunks[old_len..], incoming.hunks);
            allocator.free(existing.hunks);
            existing.hunks = merged_hunks;

            allocator.free(incoming.path);
            if (incoming.old_path) |old| allocator.free(old);
            if (incoming.language) |lang| allocator.free(lang);
            allocator.free(incoming.patch_fingerprint);
            allocator.free(incoming.patch_text);
            return;
        }
    }
    try files.append(allocator, incoming);
}

pub fn statusList(allocator: std.mem.Allocator, io: std.Io, repo_root: []const u8, target: diff.ReviewTarget, debug: bool) GitError![]u8 {
    const runner = GitRunner{ .allocator = allocator, .io = io, .repo_root = repo_root, .debug = debug };
    const args = if (target.kind == .working_tree)
        &.{ "status", "--short" }
    else
        &.{ "diff", "--name-status" };
    const result = try runner.run(args);
    allocator.free(result.stderr);
    return result.stdout;
}
