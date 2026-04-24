const std = @import("std");
const diffo = @import("diffo");

pub fn main(init: std.process.Init) !void {
    const allocator = init.gpa;
    var iterator = try std.process.Args.Iterator.initAllocator(init.minimal.args, allocator);
    defer iterator.deinit();

    _ = iterator.next();
    var args_list: std.ArrayList([]const u8) = .empty;
    defer args_list.deinit(allocator);
    while (iterator.next()) |arg| try args_list.append(allocator, arg);

    diffo.cli.run(allocator, init.io, args_list.items) catch |err| {
        const stderr = std.Io.File.stderr();
        const msg = switch (err) {
            error.NotGitRepository => "diffo: current directory is not inside a Git repository\n",
            error.GitCommandFailed => "diffo: git command failed; run with --debug-git for details\n",
            error.InvalidArguments => "diffo: invalid arguments; use diffo --help\n",
            error.StorageCorrupted => "diffo: stored review data is corrupted\n",
            error.ThemeInvalid => "diffo: theme file does not look like Base16/Base24\n",
            else => try std.fmt.allocPrint(allocator, "diffo: {s}\n", .{@errorName(err)}),
        };
        defer if (err != error.NotGitRepository and err != error.GitCommandFailed and err != error.InvalidArguments and err != error.StorageCorrupted and err != error.ThemeInvalid) allocator.free(msg);
        stderr.writeStreamingAll(init.io, msg) catch {};
        std.process.exit(1);
    };
}
