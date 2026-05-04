pub const cli = @import("cli.zig");
pub const diff = @import("diff.zig");
pub const git = @import("git.zig");
pub const inline_diff = @import("inline_diff.zig");
pub const store = @import("store.zig");
pub const syntax = @import("syntax.zig");
pub const syntax_cache = @import("syntax_cache.zig");
pub const syntax_grammars = @import("syntax_grammars.zig");
pub const syntax_query = @import("syntax_query.zig");
pub const theme = @import("theme.zig");
pub const tree_sitter = @import("tree_sitter.zig");
pub const tui = @import("tui.zig");
pub const tui_text = @import("tui_text.zig");
pub const tui_view = @import("tui_view.zig");
pub const util = @import("util.zig");

test {
    _ = cli;
    _ = diff;
    _ = git;
    _ = inline_diff;
    _ = store;
    _ = syntax;
    _ = syntax_cache;
    _ = syntax_grammars;
    _ = syntax_query;
    _ = theme;
    _ = tree_sitter;
    _ = tui;
    _ = tui_text;
    _ = tui_view;
    _ = util;
}
