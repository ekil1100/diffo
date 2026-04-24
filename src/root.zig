pub const cli = @import("cli.zig");
pub const diff = @import("diff.zig");
pub const git = @import("git.zig");
pub const store = @import("store.zig");
pub const syntax = @import("syntax.zig");
pub const theme = @import("theme.zig");
pub const tui = @import("tui.zig");
pub const tui_text = @import("tui_text.zig");
pub const tui_view = @import("tui_view.zig");
pub const util = @import("util.zig");

test {
    _ = cli;
    _ = diff;
    _ = git;
    _ = store;
    _ = syntax;
    _ = theme;
    _ = tui;
    _ = tui_text;
    _ = tui_view;
    _ = util;
}
