const std = @import("std");

pub fn build(b: *std.Build) void {
    const target = b.standardTargetOptions(.{});
    const optimize = b.standardOptimizeOption(.{});

    const mod = b.addModule("diffo", .{
        .root_source_file = b.path("src/root.zig"),
        .target = target,
        .link_libc = true,
    });
    addSyntaxNativeSources(b, mod);

    const exe = b.addExecutable(.{
        .name = "diffo",
        .root_module = b.createModule(.{
            .root_source_file = b.path("src/main.zig"),
            .target = target,
            .optimize = optimize,
            .link_libc = true,
            .imports = &.{
                .{ .name = "diffo", .module = mod },
            },
        }),
    });
    b.installArtifact(exe);

    const run_step = b.step("run", "Run diffo");
    const run_cmd = b.addRunArtifact(exe);
    run_cmd.step.dependOn(b.getInstallStep());
    if (b.args) |args| run_cmd.addArgs(args);
    run_step.dependOn(&run_cmd.step);

    const mod_tests = b.addTest(.{
        .root_module = mod,
    });
    const run_mod_tests = b.addRunArtifact(mod_tests);

    const exe_tests = b.addTest(.{
        .root_module = exe.root_module,
    });
    const run_exe_tests = b.addRunArtifact(exe_tests);

    const test_step = b.step("test", "Run tests");
    test_step.dependOn(&run_mod_tests.step);
    test_step.dependOn(&run_exe_tests.step);
}

fn addSyntaxNativeSources(b: *std.Build, module: *std.Build.Module) void {
    module.addIncludePath(b.path("vendor/tree-sitter/lib/include"));
    module.addIncludePath(b.path("vendor/tree-sitter/lib/src"));
    module.addIncludePath(b.path("vendor/tree-sitter-zig/src"));
    module.addIncludePath(b.path("vendor/tree-sitter-javascript/src"));
    module.addIncludePath(b.path("vendor/tree-sitter-typescript/typescript/src"));
    module.addIncludePath(b.path("vendor/tree-sitter-typescript/tsx/src"));
    module.addIncludePath(b.path("vendor/tree-sitter-rust/src"));
    module.addIncludePath(b.path("vendor/tree-sitter-c/src"));
    module.addIncludePath(b.path("vendor/tree-sitter-cpp/src"));
    module.addIncludePath(b.path("vendor/tree-sitter-python/src"));
    module.addCSourceFile(.{
        .file = b.path("vendor/tree-sitter/lib/src/lib.c"),
        .flags = &.{ "-std=c11", "-O2" },
    });
    addC11Source(b, module, "vendor/tree-sitter-zig/src/parser.c");
    addC11Source(b, module, "vendor/tree-sitter-javascript/src/parser.c");
    addC11Source(b, module, "vendor/tree-sitter-javascript/src/scanner.c");
    addC11Source(b, module, "vendor/tree-sitter-typescript/typescript/src/parser.c");
    addC11Source(b, module, "vendor/tree-sitter-typescript/typescript/src/scanner.c");
    addC11Source(b, module, "vendor/tree-sitter-typescript/tsx/src/parser.c");
    addC11Source(b, module, "vendor/tree-sitter-typescript/tsx/src/scanner.c");
    addC11Source(b, module, "vendor/tree-sitter-rust/src/parser.c");
    addC11Source(b, module, "vendor/tree-sitter-rust/src/scanner.c");
    addC11Source(b, module, "vendor/tree-sitter-c/src/parser.c");
    addC11Source(b, module, "vendor/tree-sitter-cpp/src/parser.c");
    addC11Source(b, module, "vendor/tree-sitter-cpp/src/scanner.c");
    addC11Source(b, module, "vendor/tree-sitter-python/src/parser.c");
    addC11Source(b, module, "vendor/tree-sitter-python/src/scanner.c");
}

fn addC11Source(b: *std.Build, module: *std.Build.Module, path: []const u8) void {
    module.addCSourceFile(.{
        .file = b.path(path),
        .flags = &.{ "-std=c11", "-O2" },
    });
}
