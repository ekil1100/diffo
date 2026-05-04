const std = @import("std");
const diff = @import("diff.zig");
const git = @import("git.zig");
const syntax = @import("syntax.zig");
const syntax_grammars = @import("syntax_grammars.zig");
const syntax_query = @import("syntax_query.zig");
const theme = @import("theme.zig");
const tree_sitter = @import("tree_sitter.zig");
const util = @import("util.zig");

pub const Error = error{
    SyntaxUnavailable,
    SourceTooLarge,
} || std.mem.Allocator.Error || git.GitError || tree_sitter.Error;

const max_file_size = 512 * 1024;

pub const SyntaxCache = struct {
    allocator: std.mem.Allocator,
    io: std.Io,
    repo: diff.Repository,
    target: diff.ReviewTarget,
    debug_git: bool,
    entries: std.ArrayList(Entry) = .empty,

    const Entry = struct {
        file_index: usize,
        side: git.FileSide,
        highlight: syntax_query.SideHighlight,

        fn deinit(self: *Entry, allocator: std.mem.Allocator) void {
            self.highlight.deinit(allocator);
        }
    };

    pub fn init(
        allocator: std.mem.Allocator,
        io: std.Io,
        repo: diff.Repository,
        target: diff.ReviewTarget,
        debug_git: bool,
    ) SyntaxCache {
        return .{
            .allocator = allocator,
            .io = io,
            .repo = repo,
            .target = target,
            .debug_git = debug_git,
        };
    }

    pub fn deinit(self: *SyntaxCache) void {
        for (self.entries.items) |*entry| entry.deinit(self.allocator);
        self.entries.deinit(self.allocator);
    }

    pub fn highlightDiffLine(
        self: *SyntaxCache,
        allocator: std.mem.Allocator,
        ansi: theme.Ansi,
        palette: theme.ThemeTokens,
        file_index: usize,
        file: diff.DiffFile,
        line: *const diff.DiffLine,
    ) Error![]u8 {
        if (!ansi.enabled) return error.SyntaxUnavailable;
        const line_ref = lineRef(line) orelse return error.SyntaxUnavailable;
        const highlight = try self.get(file_index, file, line_ref.side);
        const source_text = highlight.lineText(line_ref.line_number) orelse return error.SyntaxUnavailable;
        if (!util.eql(source_text, line.text)) return error.SyntaxUnavailable;
        return syntax.renderHighlightedLine(allocator, ansi, palette, line.text, highlight.lineSpans(line_ref.line_number));
    }

    fn get(self: *SyntaxCache, file_index: usize, file: diff.DiffFile, side: git.FileSide) Error!*syntax_query.SideHighlight {
        for (self.entries.items) |*entry| {
            if (entry.file_index == file_index and entry.side == side) return &entry.highlight;
        }
        const language = file.language orelse return error.SyntaxUnavailable;
        const grammar = syntax_grammars.find(language) orelse return error.SyntaxUnavailable;
        const source = try git.loadFileSide(self.allocator, self.io, self.repo, self.target, file, side, self.debug_git) orelse return error.SyntaxUnavailable;
        errdefer self.allocator.free(source);
        if (source.len > max_file_size) return error.SourceTooLarge;
        const highlight = try syntax_query.build(self.allocator, grammar.language(), grammar.query, source);
        try self.entries.append(self.allocator, .{
            .file_index = file_index,
            .side = side,
            .highlight = highlight,
        });
        return &self.entries.items[self.entries.items.len - 1].highlight;
    }
};

const LineRef = struct {
    side: git.FileSide,
    line_number: u32,
};

fn lineRef(line: *const diff.DiffLine) ?LineRef {
    return switch (line.kind) {
        .delete => if (line.old_lineno) |n| .{ .side = .old, .line_number = n } else null,
        .add => if (line.new_lineno) |n| .{ .side = .new, .line_number = n } else null,
        .context => if (line.new_lineno) |n|
            .{ .side = .new, .line_number = n }
        else if (line.old_lineno) |n|
            .{ .side = .old, .line_number = n }
        else
            null,
        .meta => null,
    };
}

test "line refs map diff sides" {
    var empty: [0]u8 = .{};
    var add = diff.DiffLine{ .kind = .add, .old_lineno = null, .new_lineno = 12, .text = empty[0..], .stable_line_id = empty[0..] };
    var del = diff.DiffLine{ .kind = .delete, .old_lineno = 7, .new_lineno = null, .text = empty[0..], .stable_line_id = empty[0..] };
    try std.testing.expectEqual(git.FileSide.new, lineRef(&add).?.side);
    try std.testing.expectEqual(@as(u32, 12), lineRef(&add).?.line_number);
    try std.testing.expectEqual(git.FileSide.old, lineRef(&del).?.side);
}
