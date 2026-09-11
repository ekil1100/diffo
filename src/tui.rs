use crate::{
    Error, Result,
    diff::{DiffFile, DiffLine, DiffLineKind, DiffSnapshot},
    inline_diff::{self, Range},
    store::{Comment, Store},
    syntax::{self, HighlightMode},
    syntax_cache::SyntaxCache,
    theme::{self, Ansi, Color, ThemeTokens},
    tui_text::{self, display_width, fit_cell},
    tui_view::{
        self, FileView, FoldEntry, FoldId, FoldMode, FoldState, RowKind, ViewMode, VisualRow,
    },
};
use base64::{Engine as _, engine::general_purpose::STANDARD};
use crossterm::{
    cursor::{Hide, Show},
    event::{
        DisableBracketedPaste, DisableMouseCapture, EnableBracketedPaste, EnableMouseCapture,
        Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers, MouseButton, MouseEvent,
        MouseEventKind,
    },
    execute,
    terminal::{
        self, Clear, ClearType, DisableLineWrap, EnableLineWrap, EnterAlternateScreen,
        LeaveAlternateScreen,
    },
};
use std::{
    io::{self, IsTerminal, Write},
    process::{Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
#[path = "tui_editor.rs"]
mod editor;
#[cfg(unix)]
#[path = "tui_input.rs"]
mod input;

const HELP: &str = "j/k arrows move  G/gg bottom/top  PgUp/PgDn scroll  J/K file  n/p change  C unfold/fold  z/Z folds  v view  r reviewed  c comment  V select  y copy  Esc clear  u unreviewed  ? help  q quit";
// Request enhanced keys where supported; the input decoder also accepts legacy
// modifyOtherKeys and Shift+Enter forms. Reset resources on leaving the editor.
const ENABLE_EDITOR_KEYS: &str = "\x1b[>1u\x1b[>4;1f\x1b[>4;2m";
const DISABLE_EDITOR_KEYS: &str = "\x1b[<u\x1b[>4;0m\x1b[>4f";

#[derive(Debug, Clone, Copy)]
struct Layout {
    width: usize,
    height: usize,
    body_height: usize,
    main_width: usize,
    sidebar_x: usize,
    sidebar_width: usize,
}
impl Layout {
    fn new(width: usize, height: usize) -> Self {
        // Honor actual small terminals rather than writing outside their screen.
        let width = width.max(1);
        let height = height.max(3);
        let sidebar_width = if width >= 120 {
            (width / 4).clamp(36, 52)
        } else if width >= 90 {
            32
        } else {
            0
        };
        let main_width = width - sidebar_width - usize::from(sidebar_width > 0);
        Self {
            width,
            height,
            body_height: height - 2,
            main_width,
            sidebar_x: main_width + 1,
            sidebar_width,
        }
    }
    fn terminal() -> Self {
        let (w, h) = terminal::size()
            .ok()
            .filter(|&(w, h)| w > 0 && h > 0)
            .unwrap_or_else(|| {
                fn env(name: &str, fallback: u16) -> u16 {
                    std::env::var(name)
                        .ok()
                        .and_then(|v| v.parse().ok())
                        .filter(|v| *v > 0)
                        .unwrap_or(fallback)
                }
                (env("COLUMNS", 100), env("LINES", 32))
            });
        Self::new(w as usize, h as usize)
    }
}

#[derive(Debug)]
struct State {
    active_file: usize,
    cursor_row: usize,
    scroll_row: usize,
    // Allows scrolling within a single wrapped line taller than the viewport.
    scroll_line: usize,
    mode: ViewMode,
    fold_mode: FoldMode,
    selection_start: Option<usize>,
    notice: String,
    help: bool,
    pending: Option<char>,
    folds: Vec<FoldEntry>,
    preserve_scroll_once: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            active_file: 0,
            cursor_row: 0,
            scroll_row: 0,
            scroll_line: 0,
            mode: ViewMode::Stacked,
            fold_mode: FoldMode::Unfold,
            selection_start: None,
            notice: String::new(),
            help: false,
            pending: None,
            folds: Vec::new(),
            preserve_scroll_once: false,
        }
    }
}
impl State {
    fn view<'a>(&self, snapshot: &'a DiffSnapshot) -> FileView<'a> {
        tui_view::build_file_view(
            &snapshot.files[self.active_file],
            self.active_file,
            self.mode,
            self.fold_mode,
            &self.folds,
        )
    }
    fn selected(&self, row: usize) -> bool {
        let start = self.selection_start.unwrap_or(self.cursor_row);
        (start.min(self.cursor_row)..=start.max(self.cursor_row)).contains(&row)
    }
    fn clear_selection(&mut self) {
        self.selection_start = None;
        self.notice.clear();
        self.help = false;
        self.pending = None;
    }
    fn set_fold(&mut self, id: FoldId, state: FoldState) {
        if let Some(entry) = self.folds.iter_mut().find(|entry| entry.id == id) {
            entry.state = state;
        } else {
            self.folds.push(FoldEntry { id, state });
        }
    }
}

/// Runs a review without writing state until the user explicitly reviews or comments.
pub fn run(snapshot: &DiffSnapshot, store: &mut Store, author: &str) -> Result<()> {
    if snapshot.files.is_empty() {
        io::stdout().write_all(b"diffo: no changes for this review target\n")?;
        return Ok(());
    }
    let mut state = State::default();
    let layout = Layout::terminal();
    for index in 0..snapshot.files.len() {
        state.active_file = index;
        if first_change(snapshot, store, &mut state, layout) {
            break;
        }
        if index + 1 == snapshot.files.len() {
            state.active_file = 0;
        }
    }
    let interactive = !cfg!(windows) && io::stdin().is_terminal() && io::stdout().is_terminal();
    let mut renderer = Renderer {
        cache: SyntaxCache::new(&snapshot.repository, &snapshot.review_target, false),
        ansi: if interactive {
            Ansi::init(true)
        } else {
            Ansi {
                enabled: false,
                true_color: false,
            }
        },
        palette: theme::catppuccin_mocha(),
    };
    if !interactive {
        let screen = renderer.frame(snapshot, store, &mut state, layout, false)?;
        io::stdout().write_all(screen.as_bytes())?;
        io::stdout().write_all(b"\n")?;
        return Ok(());
    }
    let mut terminal = TerminalGuard::enter()?;
    let mut editor: Option<editor::Editor> = None;
    #[cfg(unix)]
    let mut input = input::Input::default();
    'review: loop {
        if terminal.interrupted.load(Ordering::Relaxed) {
            break;
        }
        let layout = Layout::terminal();
        let mut frame = renderer.frame(snapshot, store, &mut state, layout, true)?;
        if let Some(input) = &editor {
            frame.push_str(&input.draw(layout, renderer.ansi, renderer.palette));
        }
        // End synchronized output only after the overlay has been painted.
        frame.push_str("\x1b[?2026l");
        io::stdout().write_all(frame.as_bytes())?;
        io::stdout().flush()?;
        let event = loop {
            if terminal.interrupted.load(Ordering::Relaxed) {
                break 'review;
            }
            #[cfg(unix)]
            if terminal.resized.swap(false, Ordering::Relaxed) {
                let (width, height) = terminal::size()?;
                break Event::Resize(width, height);
            }
            #[cfg(unix)]
            let event = input.read(Duration::from_millis(100));
            #[cfg(not(unix))]
            let event = crossterm::event::poll(Duration::from_millis(100)).and_then(|ready| {
                if ready {
                    crossterm::event::read().map(Some)
                } else {
                    Ok(None)
                }
            });
            match event {
                Ok(Some(event)) => break event,
                Ok(None) => continue,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        };
        if let Some(input) = editor.as_mut() {
            let action = match event {
                Event::Key(key) => input.key(key),
                Event::Paste(text) => {
                    input.paste(&text);
                    editor::Action::Continue
                }
                _ => editor::Action::Continue,
            };
            match action {
                editor::Action::Continue => {}
                editor::Action::Quit => break,
                editor::Action::Save | editor::Action::Cancel => {
                    if action == editor::Action::Save && !input.body.is_empty() {
                        state.notice =
                            if add_comment(snapshot, store, &state, &input.body, author)? {
                                "comment added"
                            } else {
                                "no diff line in current file"
                            }
                            .into();
                    }
                    editor = None;
                    state.selection_start = None;
                    terminal.editor_keys(false)?;
                    execute!(io::stdout(), EnableMouseCapture, Hide)?;
                }
            }
            continue;
        }
        match event {
            Event::Key(key) => match handle_key(snapshot, store, &mut state, key, layout)? {
                KeyAction::Quit => break,
                KeyAction::Comment => {
                    editor = Some(editor::Editor::default());
                    execute!(io::stdout(), DisableMouseCapture, Show)?;
                    terminal.editor_keys(true)?;
                }
                KeyAction::Copy => {
                    let (text, count) = selected_text(snapshot, &state);
                    state.notice = if text.is_empty() {
                        "nothing copyable at cursor".into()
                    } else {
                        match write_clipboard(&text, &terminal.interrupted) {
                            CopyMethod::System => format!(
                                "copied {count} line{} to clipboard",
                                if count == 1 { "" } else { "s" }
                            ),
                            CopyMethod::Tmux => format!(
                                "{count} lines saved to tmux buffer — Ctrl+b ] to paste; install xclip/wl-copy for system clipboard"
                            ),
                            CopyMethod::None => {
                                "clipboard unavailable; install xclip/wl-copy or enable OSC 52"
                                    .into()
                            }
                        }
                    };
                }
                KeyAction::Continue => {}
            },
            Event::Mouse(mouse) => handle_mouse(snapshot, store, &mut state, mouse, layout),
            Event::Resize(_, _) => {
                state.scroll_line = 0;
            }
            _ => {}
        }
    }
    let signal_exit_code = terminal.signal_exit_code.load(Ordering::Relaxed);
    drop(terminal);
    if signal_exit_code != 0 {
        std::process::exit(signal_exit_code as i32);
    }
    Ok(())
}

struct TerminalGuard {
    interrupted: Arc<AtomicBool>,
    signal_exit_code: Arc<AtomicUsize>,
    editor_keys_active: bool,
    raw_active: bool,
    screen_active: bool,
    #[cfg(unix)]
    signals: Vec<signal_hook::SigId>,
    #[cfg(unix)]
    resized: Arc<AtomicBool>,
}
impl TerminalGuard {
    fn enter() -> io::Result<Self> {
        let mut guard = Self {
            interrupted: Arc::new(AtomicBool::new(false)),
            signal_exit_code: Arc::new(AtomicUsize::new(0)),
            editor_keys_active: false,
            raw_active: false,
            screen_active: false,
            #[cfg(unix)]
            signals: Vec::new(),
            #[cfg(unix)]
            resized: Arc::new(AtomicBool::new(false)),
        };
        // Install cleanup ownership before any setup which can partially succeed.
        #[cfg(unix)]
        for signal in [signal_hook::consts::SIGINT, signal_hook::consts::SIGTERM] {
            guard.signals.push(signal_hook::flag::register_usize(
                signal,
                Arc::clone(&guard.signal_exit_code),
                128 + signal as usize,
            )?);
            guard.signals.push(signal_hook::flag::register(
                signal,
                Arc::clone(&guard.interrupted),
            )?);
        }
        #[cfg(unix)]
        guard.signals.push(signal_hook::flag::register(
            signal_hook::consts::SIGWINCH,
            Arc::clone(&guard.resized),
        )?);
        terminal::enable_raw_mode()?;
        guard.raw_active = true;
        guard.screen_active = true;
        execute!(
            io::stdout(),
            EnterAlternateScreen,
            DisableLineWrap,
            Clear(ClearType::All),
            Hide,
            EnableMouseCapture,
            EnableBracketedPaste
        )?;
        Ok(guard)
    }
    fn editor_keys(&mut self, enabled: bool) -> io::Result<()> {
        if enabled {
            self.editor_keys_active = true;
            io::stdout().write_all(ENABLE_EDITOR_KEYS.as_bytes())
        } else if self.editor_keys_active {
            io::stdout().write_all(DISABLE_EDITOR_KEYS.as_bytes())?;
            self.editor_keys_active = false;
            Ok(())
        } else {
            Ok(())
        }
    }
}
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        // Drop also runs during unwinding, and setup failures retain this guard.
        if self.screen_active {
            let _ = io::stdout().write_all(b"\x1b[?2026l\x1b[0m");
            let _ = self.editor_keys(false);
            let _ = execute!(
                io::stdout(),
                DisableBracketedPaste,
                DisableMouseCapture,
                Show,
                EnableLineWrap,
                LeaveAlternateScreen
            );
        }
        if self.raw_active {
            let _ = terminal::disable_raw_mode();
        }
        #[cfg(unix)]
        for id in self.signals.drain(..) {
            signal_hook::low_level::unregister(id);
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeyAction {
    Continue,
    Quit,
    Comment,
    Copy,
}
fn handle_key(
    snapshot: &DiffSnapshot,
    store: &mut Store,
    state: &mut State,
    key: KeyEvent,
    layout: Layout,
) -> Result<KeyAction> {
    if key.kind == KeyEventKind::Release {
        return Ok(KeyAction::Continue);
    }
    if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
        return Ok(KeyAction::Quit);
    }
    if key
        .modifiers
        .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT)
    {
        return Ok(KeyAction::Continue);
    }
    if key.code == KeyCode::Char('q') {
        return Ok(KeyAction::Quit);
    }
    if key.code == KeyCode::Esc {
        state.clear_selection();
        return Ok(KeyAction::Continue);
    }
    if state.help && key.code != KeyCode::Char('?') {
        state.help = false;
        state.pending = None;
        state.notice.clear();
        return Ok(KeyAction::Continue);
    }
    if key.code != KeyCode::Char('y') {
        state.notice.clear();
    }
    let pending = state.pending.take();
    match key.code {
        KeyCode::Char('g') if pending != Some('g') => state.pending = Some('g'),
        KeyCode::Char('g') | KeyCode::Home => {
            state.cursor_row = 0;
            state.scroll_line = 0;
        }
        KeyCode::Char('G') | KeyCode::End => {
            state.cursor_row = state.view(snapshot).rows.len().saturating_sub(1);
            state.scroll_line = 0;
        }
        KeyCode::Char('[') => state.pending = Some('['),
        KeyCode::Char(']') => state.pending = Some(']'),
        KeyCode::Char('f') if pending == Some('[') || pending == Some(']') => move_file(
            snapshot,
            store,
            state,
            if pending == Some('[') { -1 } else { 1 },
            layout,
        ),
        KeyCode::Char('j') | KeyCode::Down => move_line(snapshot, store, state, 1, false, layout),
        KeyCode::Char('k') | KeyCode::Up => move_line(snapshot, store, state, -1, false, layout),
        KeyCode::PageDown => move_line(snapshot, store, state, 12, true, layout),
        KeyCode::PageUp => move_line(snapshot, store, state, -12, true, layout),
        KeyCode::Char('J') => move_file(snapshot, store, state, 1, layout),
        KeyCode::Char('K') => move_file(snapshot, store, state, -1, layout),
        KeyCode::Char('n') | KeyCode::Char('p') => {
            let view = state.view(snapshot);
            let next = if key.code == KeyCode::Char('n') {
                tui_view::next_change(&view, state.cursor_row)
            } else {
                tui_view::previous_change(&view, state.cursor_row)
            };
            if let Some(row) = next {
                state.cursor_row = row;
                center(snapshot, store, &view, state, layout);
            }
        }
        KeyCode::Char('C') | KeyCode::Char('z') | KeyCode::Char('Z') => {
            toggle_folds(snapshot, store, state, key.code, layout)
        }
        KeyCode::Char('v') => {
            state.selection_start = None;
            state.mode = if state.mode == ViewMode::Stacked {
                ViewMode::Split
            } else {
                ViewMode::Stacked
            };
            state.scroll_line = 0;
        }
        KeyCode::Char('V') => state.selection_start = Some(state.cursor_row),
        KeyCode::Char('y') => return Ok(KeyAction::Copy),
        KeyCode::Char('c') => return Ok(KeyAction::Comment),
        KeyCode::Char('?') => state.help = !state.help,
        KeyCode::Char('u') => {
            if let Some(index) = snapshot.files.iter().position(|file| {
                !store.is_reviewed(
                    &file.path,
                    &file.patch_fingerprint,
                    &snapshot.review_target.target_id,
                )
            }) {
                state.active_file = index;
                state.selection_start = None;
                first_change(snapshot, store, state, layout);
            }
        }
        KeyCode::Char('r') => {
            let file = &snapshot.files[state.active_file];
            let reviewed = !store.is_reviewed(
                &file.path,
                &file.patch_fingerprint,
                &snapshot.review_target.target_id,
            );
            store.set_reviewed(
                &snapshot.repository.repo_id,
                &snapshot.review_target.target_id,
                file,
                reviewed,
            )?;
        }
        _ => {}
    }
    let view = state.view(snapshot);
    ensure_visible(
        snapshot,
        store,
        &view,
        state,
        layout,
        !state.preserve_scroll_once,
    );
    Ok(KeyAction::Continue)
}

fn move_index(current: usize, len: usize, delta: isize) -> usize {
    current
        .saturating_add_signed(delta)
        .min(len.saturating_sub(1))
}
fn move_file(
    snapshot: &DiffSnapshot,
    store: &Store,
    state: &mut State,
    delta: isize,
    layout: Layout,
) {
    state.active_file = move_index(state.active_file, snapshot.files.len(), delta);
    state.selection_start = None;
    first_change(snapshot, store, state, layout);
}
fn first_change(snapshot: &DiffSnapshot, store: &Store, state: &mut State, layout: Layout) -> bool {
    state.cursor_row = 0;
    state.scroll_row = 0;
    state.scroll_line = 0;
    let view = state.view(snapshot);
    if let Some(change) = view.changes.first() {
        state.cursor_row = change.start_row;
        center(snapshot, store, &view, state, layout);
        true
    } else {
        false
    }
}
fn move_line(
    snapshot: &DiffSnapshot,
    store: &Store,
    state: &mut State,
    delta: isize,
    scroll: bool,
    layout: Layout,
) {
    let view = state.view(snapshot);
    if view.rows.is_empty() {
        return;
    }
    if scroll && state.cursor_row == state.scroll_row {
        let height = row_height(snapshot, store, &view, state, state.cursor_row, layout);
        let max_offset = height.saturating_sub(layout.body_height);
        if (delta > 0 && state.scroll_line < max_offset) || (delta < 0 && state.scroll_line > 0) {
            state.scroll_line = state
                .scroll_line
                .saturating_add_signed(delta)
                .min(max_offset);
            return;
        }
    }
    state.cursor_row = move_index(state.cursor_row, view.rows.len(), delta);
    state.scroll_line = 0;
    if scroll {
        state.scroll_row = move_index(state.scroll_row, view.rows.len(), delta);
    }
    ensure_visible(snapshot, store, &view, state, layout, true);
}
fn file_tree_start(file_count: usize, active: usize, height: usize) -> usize {
    if height <= 2 || active < height - 1 {
        0
    } else {
        (active - (height - 2)).min(file_count)
    }
}
fn handle_mouse(
    snapshot: &DiffSnapshot,
    store: &Store,
    state: &mut State,
    mouse: MouseEvent,
    layout: Layout,
) {
    // Ignore motion-only events (crossterm enables all-motion tracking).
    if matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Up(_)) {
        return;
    }
    state.pending = None;
    state.notice.clear();
    let sidebar = layout.sidebar_width > 0 && mouse.column as usize >= layout.sidebar_x;
    let delta = match mouse.kind {
        MouseEventKind::ScrollUp => -3,
        MouseEventKind::ScrollDown => 3,
        _ => 0,
    };
    if delta != 0 {
        if sidebar {
            move_file(snapshot, store, state, delta, layout);
        } else {
            move_line(snapshot, store, state, delta, true, layout);
        }
        return;
    }
    let drag = mouse.kind == MouseEventKind::Drag(MouseButton::Left);
    if mouse.kind != MouseEventKind::Down(MouseButton::Left) && !drag {
        return;
    }
    let y = mouse.row as usize;
    if y < 1 || y >= layout.height - 1 {
        return;
    }
    if sidebar {
        if drag || y == 1 {
            return;
        }
        let index =
            file_tree_start(snapshot.files.len(), state.active_file, layout.body_height) + y - 2;
        if index < snapshot.files.len() {
            state.active_file = index;
            state.selection_start = None;
            first_change(snapshot, store, state, layout);
        }
    } else {
        let view = state.view(snapshot);
        if view.rows.is_empty() {
            return;
        }
        let row = row_at_offset(snapshot, store, &view, state, y - 1, layout);
        if drag {
            state.selection_start.get_or_insert(state.cursor_row);
        } else {
            state.selection_start = None;
        }
        state.cursor_row = row;
        ensure_visible(snapshot, store, &view, state, layout, true);
    }
}

fn line_number(line: &DiffLine) -> Option<u32> {
    match line.kind {
        DiffLineKind::Delete => line.old_lineno,
        DiffLineKind::Add => line.new_lineno,
        DiffLineKind::Context => line.new_lineno.or(line.old_lineno),
        DiffLineKind::Meta => None,
    }
}
fn number_width(file: &DiffFile) -> usize {
    file.hunks
        .iter()
        .flat_map(|h| &h.lines)
        .flat_map(|l| [l.old_lineno, l.new_lineno])
        .flatten()
        .max()
        .unwrap_or(0)
        .to_string()
        .len()
        .max(4)
}
fn wrap_widths(width: usize, prefix: usize, text: &str) -> (usize, usize, usize) {
    let indent: usize = text
        .chars()
        .take_while(|c| *c == ' ' || *c == '\t')
        .map(|c| if c == '\t' { 4 } else { 1 })
        .sum();
    let continuation = if width <= prefix + 1 {
        prefix
    } else {
        (prefix + indent + 1).min(width - 1)
    };
    (
        width.saturating_sub(prefix).max(1),
        width.saturating_sub(continuation).max(1),
        continuation,
    )
}
fn line_height(line: &DiffLine, width: usize, prefix: usize) -> usize {
    let (first, continuation, _) = wrap_widths(width, prefix, &line.text);
    wrap_ansi(&plain_text(&line.text), first, continuation).len()
}
fn comment_matches(comment: &Comment, file: &DiffFile, line: &DiffLine, target: &str) -> bool {
    let Some(number) = line_number(line) else {
        return false;
    };
    let end = if comment.end_line == 0 {
        comment.start_line
    } else {
        comment.end_line
    };
    comment.review_target_id == target
        && comment.file_path == file.path
        && (comment.start_line.min(end)..=comment.start_line.max(end)).contains(&number)
        && (line.kind == DiffLineKind::Context
            || comment.side
                == if line.kind == DiffLineKind::Delete {
                    "old"
                } else {
                    "new"
                })
}
fn row_has_comment(row: &VisualRow<'_>, comment: &Comment, file: &DiffFile, target: &str) -> bool {
    [row.line, row.left, row.right]
        .into_iter()
        .flatten()
        .any(|line| comment_matches(comment, file, line, target))
}
fn row_height(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &State,
    index: usize,
    layout: Layout,
) -> usize {
    let file = &snapshot.files[state.active_file];
    let row = &view.rows[index];
    let digits = number_width(file);
    let code_height = match row.kind {
        RowKind::StackedCode | RowKind::FileMeta => row
            .line
            .map_or(1, |line| line_height(line, layout.main_width, digits + 5)),
        RowKind::SplitCode if layout.main_width < 32 => row
            .right
            .or(row.left)
            .map_or(1, |line| line_height(line, layout.main_width, digits + 5)),
        RowKind::SplitCode => {
            let left_width = (layout.main_width - 3) / 2;
            let right_width = layout.main_width - 3 - left_width;
            row.left
                .map_or(1, |line| line_height(line, left_width, digits + 4))
                .max(
                    row.right
                        .map_or(1, |line| line_height(line, right_width, digits + 4)),
                )
        }
        _ => 1,
    };
    if !state.selected(index)
        || !matches!(
            row.kind,
            RowKind::StackedCode | RowKind::SplitCode | RowKind::FileMeta
        )
    {
        return code_height;
    }
    code_height
        + store
            .comments
            .iter()
            .filter(|comment| {
                row_has_comment(row, comment, file, &snapshot.review_target.target_id)
            })
            .map(|comment| comment.body.split('\n').count())
            .sum::<usize>()
}
fn height_between(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &State,
    start: usize,
    end: usize,
    layout: Layout,
) -> usize {
    (start..end.min(view.rows.len()))
        .map(|row| row_height(snapshot, store, view, state, row, layout))
        .sum()
}
fn scroll_for_offset(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &State,
    row: usize,
    offset: usize,
    layout: Layout,
) -> usize {
    let mut scroll = row;
    let mut height = 0;
    while scroll > 0 {
        let previous = row_height(snapshot, store, view, state, scroll - 1, layout);
        if height + previous > offset {
            break;
        }
        height += previous;
        scroll -= 1;
    }
    scroll
}
fn center(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &mut State,
    layout: Layout,
) {
    state.scroll_row = scroll_for_offset(
        snapshot,
        store,
        view,
        state,
        state.cursor_row,
        layout.body_height / 2,
        layout,
    );
    state.scroll_line = 0;
    state.preserve_scroll_once = true;
    ensure_visible(snapshot, store, view, state, layout, false);
}
fn ensure_visible(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &mut State,
    layout: Layout,
    fill_bottom: bool,
) {
    if view.rows.is_empty() {
        state.cursor_row = 0;
        state.scroll_row = 0;
        state.scroll_line = 0;
        return;
    }
    state.cursor_row = state.cursor_row.min(view.rows.len() - 1);
    state.scroll_row = state.scroll_row.min(state.cursor_row);
    if state.scroll_row < state.cursor_row {
        state.scroll_line = 0;
        // Only walk to the cursor until the screen is full.
        let mut height = 0;
        let mut overflow = false;
        for index in state.scroll_row..=state.cursor_row {
            height += row_height(snapshot, store, view, state, index, layout);
            if height > layout.body_height {
                overflow = true;
                break;
            }
        }
        if overflow {
            let cursor_height = row_height(snapshot, store, view, state, state.cursor_row, layout);
            state.scroll_row = scroll_for_offset(
                snapshot,
                store,
                view,
                state,
                state.cursor_row,
                layout.body_height.saturating_sub(cursor_height),
                layout,
            );
        }
    }
    state.scroll_line = state.scroll_line.min(
        row_height(snapshot, store, view, state, state.scroll_row, layout)
            .saturating_sub(layout.body_height),
    );
    if fill_bottom && state.scroll_line == 0 {
        let mut height = 0;
        for row in state.scroll_row..view.rows.len() {
            height += row_height(snapshot, store, view, state, row, layout);
            if height >= layout.body_height {
                break;
            }
        }
        while state.scroll_row > 0 && height < layout.body_height {
            let previous = row_height(snapshot, store, view, state, state.scroll_row - 1, layout);
            if height + previous > layout.body_height {
                break;
            }
            height += previous;
            state.scroll_row -= 1;
        }
    }
}
fn row_at_offset(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &State,
    offset: usize,
    layout: Layout,
) -> usize {
    let mut y = 0;
    for row in state.scroll_row..view.rows.len() {
        y += row_height(snapshot, store, view, state, row, layout);
        if offset + state.scroll_line < y {
            return row;
        }
    }
    view.rows.len().saturating_sub(1)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CodeAnchor {
    line: Option<String>,
    left: Option<String>,
    right: Option<String>,
}
fn code_anchor(row: &VisualRow<'_>) -> Option<CodeAnchor> {
    if !matches!(row.kind, RowKind::StackedCode | RowKind::SplitCode) {
        return None;
    }
    if row.line.is_none() && row.left.is_none() && row.right.is_none() {
        return None;
    }
    Some(CodeAnchor {
        line: row.line.map(|l| l.stable_line_id.clone()),
        left: row.left.map(|l| l.stable_line_id.clone()),
        right: row.right.map(|l| l.stable_line_id.clone()),
    })
}
fn capture_anchor(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &State,
    layout: Layout,
) -> Option<(CodeAnchor, usize)> {
    let capture = |index: usize| {
        let anchor = code_anchor(view.rows.get(index)?)?;
        let offset = if index < state.scroll_row {
            0
        } else {
            height_between(
                snapshot,
                store,
                view,
                state,
                state.scroll_row,
                index,
                layout,
            )
            .saturating_sub(state.scroll_line)
        };
        Some((anchor, offset))
    };
    if let Some(anchor) = capture(state.cursor_row) {
        return Some(anchor);
    }
    let skip = view
        .rows
        .get(state.cursor_row)
        .filter(|r| r.kind == RowKind::Fold)
        .and_then(|r| r.fold_id);
    for index in state.cursor_row.max(state.scroll_row)..view.rows.len() {
        let offset = height_between(
            snapshot,
            store,
            view,
            state,
            state.scroll_row,
            index,
            layout,
        )
        .saturating_sub(state.scroll_line);
        if offset >= layout.body_height {
            break;
        }
        if skip.is_some() && view.rows[index].fold_id == skip {
            continue;
        }
        if let Some(anchor) = capture(index) {
            return Some(anchor);
        }
    }
    for index in (state.scroll_row..state.cursor_row.min(view.rows.len())).rev() {
        if let Some(anchor) = capture(index)
            && anchor.1 < layout.body_height
        {
            return Some(anchor);
        }
    }
    None
}
fn toggle_folds(
    snapshot: &DiffSnapshot,
    store: &Store,
    state: &mut State,
    key: KeyCode,
    layout: Layout,
) {
    if key != KeyCode::Char('C') && state.fold_mode == FoldMode::Unfold {
        return;
    }
    let view = state.view(snapshot);
    let target = view
        .rows
        .get(state.cursor_row)
        .filter(|row| row.kind == RowKind::Fold)
        .and_then(|row| row.fold_id);
    if key == KeyCode::Char('z') && target.is_none() {
        return;
    }
    let anchor = capture_anchor(snapshot, store, &view, state, layout);
    state.selection_start = None;
    state.scroll_line = 0;
    match key {
        KeyCode::Char('C') => {
            state.fold_mode = if state.fold_mode == FoldMode::Unfold {
                FoldMode::Fold
            } else {
                FoldMode::Unfold
            };
            if state.fold_mode == FoldMode::Fold {
                state.folds.clear();
            }
        }
        KeyCode::Char('z') => {
            let id = target.unwrap();
            let next = if tui_view::fold_state(&state.folds, id) == FoldState::Collapsed {
                FoldState::Expanded
            } else {
                FoldState::Collapsed
            };
            state.set_fold(id, next);
        }
        KeyCode::Char('Z') => {
            let expand = view
                .rows
                .iter()
                .any(|row| row.kind == RowKind::Fold && !row.fold_expanded);
            for row in &view.rows {
                if row.kind == RowKind::Fold
                    && let Some(id) = row.fold_id
                {
                    state.set_fold(
                        id,
                        if expand {
                            FoldState::Expanded
                        } else {
                            FoldState::Collapsed
                        },
                    );
                }
            }
        }
        _ => return,
    }
    let updated = state.view(snapshot);
    if let Some((anchor, offset)) = anchor
        && let Some(index) = updated
            .rows
            .iter()
            .position(|row| code_anchor(row).as_ref() == Some(&anchor))
    {
        state.cursor_row = index;
        state.scroll_row =
            scroll_for_offset(snapshot, store, &updated, state, index, offset, layout);
        state.preserve_scroll_once = true;
        ensure_visible(snapshot, store, &updated, state, layout, false);
        return;
    }
    if let Some(id) = target
        && let Some(index) = updated
            .rows
            .iter()
            .position(|row| row.kind == RowKind::Fold && row.fold_id == Some(id))
    {
        state.cursor_row = index;
    }
    ensure_visible(snapshot, store, &updated, state, layout, true);
}

fn display_path(file: &DiffFile) -> String {
    match &file.old_path {
        Some(old) if old != &file.path => format!("{old} -> {}", file.path),
        _ => file.path.clone(),
    }
}
fn file_stats(view: &FileView<'_>) -> String {
    match (view.deletions, view.additions) {
        (0, 0) => String::new(),
        (0, a) => format!("+{a}"),
        (d, 0) => format!("-{d}"),
        (d, a) => format!("-{d} +{a}"),
    }
}
fn selected_text(snapshot: &DiffSnapshot, state: &State) -> (String, usize) {
    let view = state.view(snapshot);
    if view.rows.is_empty() {
        return (String::new(), 0);
    }
    let cursor = state.cursor_row.min(view.rows.len() - 1);
    let start = state
        .selection_start
        .unwrap_or(cursor)
        .min(view.rows.len() - 1);
    let file = &snapshot.files[state.active_file];
    let mut lines = Vec::new();
    for row in &view.rows[start.min(cursor)..=start.max(cursor)] {
        match row.kind {
            RowKind::FileHeader => {
                let stats = file_stats(&view);
                lines.push(if stats.is_empty() {
                    display_path(file)
                } else {
                    format!("{} ({stats})", display_path(file))
                });
            }
            RowKind::HunkHeader => lines.push(row.hunk_header.unwrap_or("").into()),
            RowKind::Fold => lines.push(format!(
                "... {} {} lines",
                row.fold_line_count,
                if row.fold_expanded {
                    "context"
                } else {
                    "hidden"
                }
            )),
            RowKind::StackedCode | RowKind::FileMeta => {
                if let Some(line) = row.line {
                    lines.push(copy_line(line));
                }
            }
            RowKind::SplitCode => {
                if let Some(left) = row.left {
                    lines.push(copy_line(left));
                }
                if let Some(right) = row.right
                    && !row.left.is_some_and(|left| std::ptr::eq(left, right))
                {
                    lines.push(copy_line(right));
                }
            }
        }
    }
    let count = lines.len();
    (
        lines
            .iter()
            .map(|line| plain_text(line))
            .collect::<Vec<_>>()
            .join("\n"),
        count,
    )
}
fn copy_line(line: &DiffLine) -> String {
    let marker = match line.kind {
        DiffLineKind::Add => "+",
        DiffLineKind::Delete => "-",
        DiffLineKind::Context => " ",
        DiffLineKind::Meta => "",
    };
    format!("{marker}{}", line.text)
}
fn comment_anchor<'a>(
    file: &'a DiffFile,
    view: &FileView<'a>,
    state: &State,
) -> Option<(&'a DiffLine, &'a str, u32)> {
    if view.rows.is_empty() {
        return None;
    }
    let cursor = state.cursor_row.min(view.rows.len() - 1);
    let start = state
        .selection_start
        .unwrap_or(cursor)
        .min(view.rows.len() - 1);
    let anchor = |row: &VisualRow<'a>| {
        let line = row.comment_line()?;
        Some((
            line,
            row.hunk_index
                .and_then(|i| file.hunks.get(i))
                .map_or("", |h| h.header.as_str()),
            line_number(line).unwrap_or(0),
        ))
    };
    let mut selected = view.rows[start.min(cursor)..=start.max(cursor)]
        .iter()
        .filter_map(anchor);
    if let Some((line, header, end)) = selected.next() {
        return Some((
            line,
            header,
            selected.next_back().map_or(end, |(_, _, end)| end),
        ));
    }
    for distance in 0..view.rows.len() {
        if let Some(found) = view.rows.get(cursor + distance).and_then(anchor) {
            return Some(found);
        }
        if distance > 0
            && let Some(found) = cursor
                .checked_sub(distance)
                .and_then(|index| view.rows.get(index))
                .and_then(anchor)
        {
            return Some(found);
        }
    }
    None
}
fn add_comment(
    snapshot: &DiffSnapshot,
    store: &mut Store,
    state: &State,
    body: &str,
    author: &str,
) -> Result<bool> {
    let file = &snapshot.files[state.active_file];
    let view = state.view(snapshot);
    let Some((line, header, end)) = comment_anchor(file, &view, state) else {
        return Ok(false);
    };
    store.add_comment(
        &snapshot.repository.repo_id,
        &snapshot.review_target.target_id,
        file,
        line,
        header,
        end,
        body,
        author,
    )?;
    Ok(true)
}

#[derive(Debug, PartialEq, Eq)]
enum CopyMethod {
    System,
    Tmux,
    None,
}
fn piped_command(argv: &[&str], text: &str, interrupted: &AtomicBool) -> bool {
    if interrupted.load(Ordering::Relaxed) {
        return false;
    }
    let Ok(mut child) = Command::new(argv[0])
        .args(&argv[1..])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    else {
        return false;
    };
    // Feed stdin on a scoped worker so a stalled clipboard program does not prevent
    // SIGTERM cleanup. No source text is ever passed to a shell or command arguments.
    let mut input = child.stdin.take().expect("piped stdin");
    std::thread::scope(|scope| {
        let writer = scope.spawn(move || input.write_all(text.as_bytes()));
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        let success = loop {
            if interrupted.load(Ordering::Relaxed) || std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                break false;
            }
            match child.try_wait() {
                Ok(Some(status)) => break status.success(),
                Ok(None) => std::thread::sleep(Duration::from_millis(10)),
                Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break false;
                }
            }
        };
        writer.join().is_ok_and(|result| result.is_ok()) && success
    })
}
fn osc52(text: &str) -> String {
    format!("\x1b]52;c;{}\x07", STANDARD.encode(text))
}
fn write_clipboard(text: &str, interrupted: &AtomicBool) -> CopyMethod {
    for argv in [
        &["wl-copy"][..],
        &["pbcopy"][..],
        &["xclip", "-selection", "clipboard"][..],
        &["xsel", "--clipboard", "--input"][..],
    ] {
        if piped_command(argv, text, interrupted) {
            return CopyMethod::System;
        }
    }
    if piped_command(&["tmux", "load-buffer", "-"], text, interrupted) {
        return CopyMethod::Tmux;
    }
    if interrupted.load(Ordering::Relaxed) {
        return CopyMethod::None;
    }
    if io::stdout()
        .write_all(osc52(text).as_bytes())
        .and_then(|_| io::stdout().flush())
        .is_ok()
    {
        CopyMethod::System
    } else {
        CopyMethod::None
    }
}

/// Removes all terminal sequences, including SGR, from untrusted source and clipboard text.
fn plain_text(text: &str) -> String {
    let sanitized = tui_text::sanitize(text);
    let mut out = String::with_capacity(sanitized.len());
    let mut rest = sanitized.as_str();
    while !rest.is_empty() {
        if rest.as_bytes()[0] == 27 {
            let len = tui_text::ansi_seq_len(rest.as_bytes());
            rest = &rest[len.max(1).min(rest.len())..];
        } else {
            let c = rest.chars().next().unwrap();
            if !c.is_control() {
                out.push(c);
            }
            rest = &rest[c.len_utf8()..];
        }
    }
    out
}

// Track independent SGR channels so wrapped continuations retain syntax and inline colors
// without accumulating an unbounded history of escape sequences.
#[derive(Default)]
struct SgrState {
    fg: String,
    bg: String,
    attrs: [bool; 10],
}
impl SgrState {
    fn apply(&mut self, sequence: &str) {
        let Some(params) = sequence
            .strip_prefix("\x1b[")
            .and_then(|s| s.strip_suffix('m'))
        else {
            return;
        };
        let parts: Vec<u16> = if params.is_empty() {
            vec![0]
        } else {
            params
                .split(';')
                .map(|part| part.parse().unwrap_or(0))
                .collect()
        };
        let mut i = 0;
        while i < parts.len() {
            match parts[i] {
                0 => {
                    self.fg.clear();
                    self.bg.clear();
                    self.attrs.fill(false);
                }
                1..=9 => self.attrs[parts[i] as usize] = true,
                22 => {
                    self.attrs[1] = false;
                    self.attrs[2] = false;
                }
                23..=29 => self.attrs[(parts[i] - 20) as usize] = false,
                30..=37 | 90..=97 => self.fg = format!("\x1b[{}m", parts[i]),
                40..=47 | 100..=107 => self.bg = format!("\x1b[{}m", parts[i]),
                39 => self.fg.clear(),
                49 => self.bg.clear(),
                38 | 48 => {
                    let count = match parts.get(i + 1) {
                        Some(2) => 5,
                        Some(5) => 3,
                        _ => 1,
                    };
                    if i + count <= parts.len() {
                        let code = format!(
                            "\x1b[{}m",
                            parts[i..i + count]
                                .iter()
                                .map(u16::to_string)
                                .collect::<Vec<_>>()
                                .join(";")
                        );
                        if parts[i] == 38 {
                            self.fg = code;
                        } else {
                            self.bg = code;
                        }
                        i += count - 1;
                    }
                }
                _ => {}
            }
            i += 1;
        }
    }
    fn code(&self) -> String {
        let mut out = String::new();
        for (i, enabled) in self.attrs.iter().enumerate() {
            if *enabled {
                out.push_str(&format!("\x1b[{i}m"));
            }
        }
        out.push_str(&self.fg);
        out.push_str(&self.bg);
        out
    }
    fn consume(&mut self, text: &str) {
        let mut rest = text;
        while !rest.is_empty() {
            if rest.as_bytes()[0] == 27 {
                let len = tui_text::ansi_seq_len(rest.as_bytes())
                    .max(1)
                    .min(rest.len());
                self.apply(&rest[..len]);
                rest = &rest[len..];
            } else {
                rest = &rest[rest.chars().next().unwrap().len_utf8()..];
            }
        }
    }
}

fn wrap_ansi(text: &str, first_width: usize, continuation_width: usize) -> Vec<String> {
    let text = tui_text::sanitize(text);
    let mut rows = Vec::new();
    let mut start = 0;
    let mut style = SgrState::default();
    while start < text.len() {
        let limit = if rows.is_empty() {
            first_width
        } else {
            continuation_width
        }
        .max(1);
        let mut i = start;
        let mut visible = 0;
        let mut seen_nonspace = false;
        let mut previous_space = false;
        let mut breakpoint: Option<(usize, usize)> = None;
        let (end, next) = loop {
            if i == text.len() {
                break (i, i);
            }
            if text.as_bytes()[i] == 27 {
                i += tui_text::ansi_seq_len(&text.as_bytes()[i..])
                    .max(1)
                    .min(text.len() - i);
                continue;
            }
            let c = text[i..].chars().next().unwrap();
            let width = display_width(&c.to_string());
            if visible > 0 && visible + width > limit {
                break breakpoint.filter(|(end, _)| *end > start).unwrap_or((i, i));
            }
            let char_start = i;
            i += c.len_utf8();
            visible += width;
            if c == ' ' {
                if seen_nonspace {
                    let end = if previous_space {
                        breakpoint.map_or(char_start, |(end, _)| end)
                    } else {
                        char_start
                    };
                    breakpoint = Some((end, i));
                }
                previous_space = true;
            } else {
                seen_nonspace = true;
                previous_space = false;
                if matches!(c, ',' | ';' | ':' | '=' | ')' | ']' | '}' | '{') {
                    breakpoint = Some((i, i));
                }
            }
        };
        let mut rendered = style.code();
        rendered.push_str(&text[start..end]);
        rows.push(rendered);
        style.consume(&text[start..next]);
        debug_assert!(next > start);
        start = next;
    }
    if rows.is_empty() {
        rows.push(String::new());
    }
    rows
}

fn row_bg(selected: bool, kind: DiffLineKind, p: ThemeTokens) -> Color {
    match (selected, kind) {
        (true, DiffLineKind::Add) => Color { hex: "#285343" },
        (true, DiffLineKind::Delete) => Color { hex: "#562932" },
        (true, _) => p.bg_selected,
        (false, DiffLineKind::Add) => p.diff_add_bg,
        (false, DiffLineKind::Delete) => p.diff_del_bg,
        _ => p.bg_default,
    }
}
fn inline_bg(selected: bool, kind: DiffLineKind) -> Color {
    Color {
        hex: match (selected, kind) {
            (true, DiffLineKind::Add) => "#3c7a5a",
            (true, DiffLineKind::Delete) => "#874151",
            (false, DiffLineKind::Add) => "#2f6f4f",
            (false, DiffLineKind::Delete) => "#74303d",
            _ => "#45475a",
        },
    }
}
fn row_fg(kind: DiffLineKind, p: ThemeTokens) -> Color {
    match kind {
        DiffLineKind::Add => p.diff_add_fg,
        DiffLineKind::Delete => p.diff_del_fg,
        DiffLineKind::Context => p.diff_context_fg,
        DiffLineKind::Meta => p.fg_muted,
    }
}
fn style_cell(text: &str, width: usize, ansi: Ansi, bg: Color, fg: Color) -> String {
    let fitted = fit_cell(text, width);
    if !ansi.enabled {
        return plain_text(&fitted);
    }
    let bg = ansi.bg(bg);
    let fg = ansi.fg(fg);
    let reset = ansi.reset();
    let restored = if reset.is_empty() {
        fitted
    } else {
        fitted.replace(reset, &format!("{reset}{bg}{fg}"))
    };
    format!("{bg}{fg}{restored}{reset}")
}
fn apply_inline(
    highlighted: &str,
    ranges: &[Range],
    ansi: Ansi,
    base: Color,
    inline: Color,
) -> String {
    if ranges.is_empty() || !ansi.enabled || !ansi.true_color {
        return highlighted.into();
    }
    let mut out = String::new();
    let mut rest = highlighted;
    let mut offset = 0;
    let mut active = false;
    let mut index = 0;
    let base_code = ansi.bg(base);
    let inline_code = ansi.bg(inline);
    while !rest.is_empty() {
        if rest.as_bytes()[0] == 27 {
            let len = tui_text::ansi_seq_len(rest.as_bytes())
                .max(1)
                .min(rest.len());
            let sequence = &rest[..len];
            out.push_str(sequence);
            if active && (sequence == ansi.reset() || sequence == "\x1b[m") {
                out.push_str(&inline_code);
            }
            rest = &rest[len..];
            continue;
        }
        while index < ranges.len() && offset >= ranges[index].end {
            index += 1;
        }
        let next_active =
            index < ranges.len() && offset >= ranges[index].start && offset < ranges[index].end;
        if active != next_active {
            out.push_str(if next_active {
                &inline_code
            } else {
                &base_code
            });
            active = next_active;
        }
        let c = rest.chars().next().unwrap();
        out.push(c);
        offset += c.len_utf8();
        rest = &rest[c.len_utf8()..];
    }
    if active {
        out.push_str(&base_code);
    }
    out
}

struct Renderer {
    cache: SyntaxCache,
    ansi: Ansi,
    palette: ThemeTokens,
}
impl Renderer {
    fn panel(&self, text: &str, width: usize, selected: bool) -> String {
        style_cell(
            &plain_text(text),
            width,
            self.ansi,
            if selected {
                self.palette.bg_selected
            } else {
                self.palette.bg_panel
            },
            self.palette.fg_muted,
        )
    }
    fn frame(
        &mut self,
        snapshot: &DiffSnapshot,
        store: &Store,
        state: &mut State,
        layout: Layout,
        interactive: bool,
    ) -> Result<String> {
        let view = state.view(snapshot);
        ensure_visible(
            snapshot,
            store,
            &view,
            state,
            layout,
            !state.preserve_scroll_once,
        );
        state.preserve_scroll_once = false;
        let file = &snapshot.files[state.active_file];
        let target = &snapshot.review_target.target_id;
        let reviewed = snapshot
            .files
            .iter()
            .filter(|file| store.is_reviewed(&file.path, &file.patch_fingerprint, target))
            .count();
        let status = format!(
            " diffo  {}  {}  files {}/{} reviewed  file {}/{}  {}",
            snapshot.repository.current_branch,
            snapshot.review_target.normalized_spec,
            reviewed,
            snapshot.files.len(),
            state.active_file + 1,
            snapshot.files.len(),
            file.path
        );
        let mut lines = Vec::with_capacity(layout.height);
        lines.push(style_cell(
            &plain_text(&status),
            layout.width,
            self.ansi,
            self.palette.bg_panel,
            self.palette.fg_default,
        ));
        let mut diff_rows = Vec::new();
        for index in state.scroll_row..view.rows.len() {
            let rendered = self.row(snapshot, store, state, &view, index, layout.main_width)?;
            let skip = if index == state.scroll_row {
                state.scroll_line
            } else {
                0
            };
            diff_rows.extend(
                rendered
                    .into_iter()
                    .skip(skip)
                    .take(layout.body_height - diff_rows.len()),
            );
            if diff_rows.len() >= layout.body_height {
                break;
            }
        }
        let tree_start =
            file_tree_start(snapshot.files.len(), state.active_file, layout.body_height);
        for y in 0..layout.body_height {
            let mut line = diff_rows.get(y).cloned().unwrap_or_else(|| {
                style_cell(
                    "",
                    layout.main_width,
                    self.ansi,
                    self.palette.bg_default,
                    self.palette.fg_default,
                )
            });
            if layout.sidebar_width > 0 {
                line.push_str(&style_cell(
                    "│",
                    1,
                    self.ansi,
                    self.palette.bg_default,
                    self.palette.border,
                ));
                let tree = if y == 0 {
                    self.panel(" files", layout.sidebar_width, false)
                } else if let Some(file) = snapshot.files.get(tree_start + y - 1) {
                    let index = tree_start + y - 1;
                    let reviewed = store.is_reviewed(&file.path, &file.patch_fingerprint, target);
                    let path = fit_cell(
                        &plain_text(&file.path),
                        layout.sidebar_width.saturating_sub(12),
                    );
                    let raw = format!(
                        "{}{} {} {} ({})",
                        if index == state.active_file { ">" } else { " " },
                        file.status.label(),
                        if reviewed { "x" } else { "." },
                        path,
                        store.comment_count(&file.path, target)
                    );
                    style_cell(
                        &raw,
                        layout.sidebar_width,
                        self.ansi,
                        if index == state.active_file {
                            self.palette.bg_selected
                        } else {
                            self.palette.bg_default
                        },
                        if reviewed {
                            self.palette.reviewed_badge
                        } else {
                            self.palette.unreviewed_badge
                        },
                    )
                } else {
                    style_cell(
                        "",
                        layout.sidebar_width,
                        self.ansi,
                        self.palette.bg_default,
                        self.palette.fg_default,
                    )
                };
                line.push_str(&tree);
            }
            lines.push(line);
        }
        let syntax = if file.is_binary {
            HighlightMode::Disabled
        } else {
            syntax::mode_for_language(file.language.as_deref())
        };
        let middle = format!(
            "mode={}/{} target={} syntax={}",
            state.mode.label(),
            state.fold_mode.label(),
            snapshot.review_target.normalized_spec,
            syntax.label()
        );
        let footer = if state.help {
            HELP.into()
        } else if state.notice.is_empty() {
            format!("{middle}  ? help  C unfold/fold  z/Z folds  q quit")
        } else {
            format!("{}  {middle}  V select  y copy", state.notice)
        };
        lines.push(style_cell(
            &plain_text(&footer),
            layout.width,
            self.ansi,
            self.palette.bg_panel,
            self.palette.fg_default,
        ));
        if interactive {
            let mut out = String::from("\x1b[?2026h\x1b[?25l");
            // Absolute positioning avoids raw-mode LF handling and last-column auto-wrap.
            for (y, line) in lines.iter().enumerate() {
                out.push_str(&format!("\x1b[{};1H{}\x1b[K", y + 1, line));
            }
            Ok(out)
        } else {
            Ok(lines.join("\n"))
        }
    }
    fn row(
        &mut self,
        snapshot: &DiffSnapshot,
        store: &Store,
        state: &State,
        view: &FileView<'_>,
        index: usize,
        width: usize,
    ) -> Result<Vec<String>> {
        let row = &view.rows[index];
        let selected = state.selected(index);
        let file = &snapshot.files[state.active_file];
        let target = &snapshot.review_target.target_id;
        let mut rows = match row.kind {
            RowKind::FileHeader => {
                let path = plain_text(&display_path(file));
                let stats = file_stats(view);
                let gap = width
                    .saturating_sub(2 + display_width(&path) + display_width(&stats) + 1)
                    .max(1);
                vec![style_cell(
                    &format!("  {path}{}{stats} ", " ".repeat(gap)),
                    width,
                    self.ansi,
                    self.palette.bg_panel,
                    self.palette.fg_default,
                )]
            }
            RowKind::HunkHeader => vec![self.panel(row.hunk_header.unwrap_or(""), width, selected)],
            RowKind::Fold => vec![self.panel(
                &format!(
                    " {}  {} {} lines",
                    if row.fold_expanded { "v" } else { ">" },
                    row.fold_line_count,
                    if row.fold_expanded {
                        "context"
                    } else {
                        "hidden"
                    }
                ),
                width,
                selected,
            )],
            RowKind::StackedCode | RowKind::FileMeta => self.code_rows(
                file,
                state.active_file,
                store,
                target,
                row.line,
                width,
                selected,
                true,
                &[],
            )?,
            RowKind::SplitCode if width < 32 => self.code_rows(
                file,
                state.active_file,
                store,
                target,
                row.right.or(row.left),
                width,
                selected,
                true,
                &[],
            )?,
            RowKind::SplitCode => {
                let pair = match (row.left, row.right) {
                    (Some(left), Some(right))
                        if left.kind == DiffLineKind::Delete && right.kind == DiffLineKind::Add =>
                    {
                        Some(inline_diff::diff_ranges(&left.text, &right.text))
                    }
                    _ => None,
                };
                let left_width = (width - 3) / 2;
                let right_width = width - 3 - left_width;
                let left = self.code_rows(
                    file,
                    state.active_file,
                    store,
                    target,
                    row.left,
                    left_width,
                    selected,
                    false,
                    pair.as_ref().map_or(&[], |p| &p.old),
                )?;
                let right = self.code_rows(
                    file,
                    state.active_file,
                    store,
                    target,
                    row.right,
                    right_width,
                    selected,
                    false,
                    pair.as_ref().map_or(&[], |p| &p.new),
                )?;
                let blank_left = style_cell(
                    "",
                    left_width,
                    self.ansi,
                    row_bg(selected, DiffLineKind::Context, self.palette),
                    self.palette.fg_default,
                );
                let blank_right = style_cell(
                    "",
                    right_width,
                    self.ansi,
                    row_bg(selected, DiffLineKind::Context, self.palette),
                    self.palette.fg_default,
                );
                let separator = style_cell(
                    " │ ",
                    3,
                    self.ansi,
                    self.palette.bg_default,
                    self.palette.border,
                );
                (0..left.len().max(right.len()))
                    .map(|i| {
                        format!(
                            "{}{separator}{}",
                            left.get(i).unwrap_or(&blank_left),
                            right.get(i).unwrap_or(&blank_right)
                        )
                    })
                    .collect()
            }
        };
        if selected
            && matches!(
                row.kind,
                RowKind::StackedCode | RowKind::SplitCode | RowKind::FileMeta
            )
        {
            for comment in &store.comments {
                if !row_has_comment(row, comment, file, target) {
                    continue;
                }
                for (i, text) in comment.body.split('\n').enumerate() {
                    let raw = if i == 0 {
                        format!(
                            "  ! {} [{}] {text}",
                            comment.author,
                            comment.match_status.label()
                        )
                    } else {
                        format!("    {text}")
                    };
                    rows.push(style_cell(
                        &plain_text(&raw),
                        width,
                        self.ansi,
                        self.palette.bg_panel,
                        self.palette.comment_badge,
                    ));
                }
            }
        }
        Ok(rows)
    }
    #[allow(clippy::too_many_arguments)]
    fn code_rows(
        &mut self,
        file: &DiffFile,
        index: usize,
        store: &Store,
        target: &str,
        line: Option<&DiffLine>,
        width: usize,
        selected: bool,
        stacked: bool,
        ranges: &[Range],
    ) -> Result<Vec<String>> {
        let Some(line) = line else {
            return Ok(vec![style_cell(
                "",
                width,
                self.ansi,
                row_bg(selected, DiffLineKind::Context, self.palette),
                self.palette.fg_muted,
            )]);
        };
        let digits = number_width(file);
        let label =
            line_number(line).map_or_else(|| " ".repeat(digits), |n| format!("{n:>digits$}"));
        let mark = if store
            .comments
            .iter()
            .any(|c| comment_matches(c, file, line, target))
        {
            "!"
        } else {
            " "
        };
        let prefix = if stacked {
            format!(
                "{}{mark} {label}  ",
                if matches!(line.kind, DiffLineKind::Add | DiffLineKind::Delete) {
                    "|"
                } else {
                    " "
                }
            )
        } else {
            format!("{mark} {label}  ")
        };
        let (first, continuation, continuation_prefix) =
            wrap_widths(width, display_width(&prefix), &line.text);
        let source = plain_text(&line.text);
        // Untrusted source SGR must not become presentation commands; if sanitizing changes
        // the source, render it plainly instead of offsetting tree-sitter spans incorrectly.
        let source_is_safe = line.text.chars().all(|c| !c.is_control() || c == '\t');
        let highlighted = if !source_is_safe || !self.ansi.enabled {
            source
        } else {
            match self
                .cache
                .highlight_diff_line(self.ansi, self.palette, index, file, line)
            {
                Ok(text) => text,
                Err(Error::SyntaxUnavailable | Error::SourceTooLarge) => source,
                Err(error) => return Err(error),
            }
        };
        let bg = row_bg(selected, line.kind, self.palette);
        let highlighted = if source_is_safe {
            apply_inline(
                &highlighted,
                ranges,
                self.ansi,
                bg,
                inline_bg(selected, line.kind),
            )
        } else {
            highlighted
        };
        let wrapped = wrap_ansi(&highlighted, first, continuation);
        Ok(wrapped
            .into_iter()
            .enumerate()
            .map(|(i, text)| {
                let raw = format!(
                    "{}{text}",
                    if i == 0 {
                        prefix.clone()
                    } else {
                        " ".repeat(continuation_prefix)
                    }
                );
                style_cell(&raw, width, self.ansi, bg, row_fg(line.kind, self.palette))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::{self, DiffSource, Repository};
    use tempfile::TempDir;

    const PATCH: &str = "diff --git a/sample.txt b/sample.txt\n--- a/sample.txt\n+++ b/sample.txt\n@@ -1,2 +1,4 @@\n keep\n-old\n+new\n+three\n+four\n";
    const ANSI: Ansi = Ansi {
        enabled: true,
        true_color: true,
    };
    const PLAIN: Ansi = Ansi {
        enabled: false,
        true_color: false,
    };
    struct Fixture {
        _dir: TempDir,
        snapshot: DiffSnapshot,
        store: Store,
    }
    impl Fixture {
        fn new(patch: &str) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::at(dir.path().join("state")).unwrap();
            let snapshot = DiffSnapshot {
                snapshot_id: "test-snapshot".into(),
                repository: Repository {
                    root_path: dir.path().join("repo"),
                    repo_id: "test-repo".into(),
                    current_branch: "main".into(),
                },
                review_target: diff::make_review_target(&[]),
                files: diff::parse_patch(patch.as_bytes(), DiffSource::Untracked).unwrap(),
            };
            Self {
                _dir: dir,
                snapshot,
                store,
            }
        }
        fn renderer(&self, ansi: Ansi) -> Renderer {
            Renderer {
                cache: SyntaxCache::new(
                    &self.snapshot.repository,
                    &self.snapshot.review_target,
                    false,
                ),
                ansi,
                palette: theme::catppuccin_mocha(),
            }
        }
        fn row(&self, state: &State, text: &str) -> usize {
            state
                .view(&self.snapshot)
                .rows
                .iter()
                .position(|r| r.comment_line().is_some_and(|l| l.text == text))
                .unwrap()
        }
        fn key(&mut self, state: &mut State, code: KeyCode, layout: Layout) -> KeyAction {
            handle_key(
                &self.snapshot,
                &mut self.store,
                state,
                KeyEvent::new(code, KeyModifiers::NONE),
                layout,
            )
            .unwrap()
        }
    }
    fn long_patch(count: usize, changes: &[usize]) -> String {
        let mut patch = format!(
            "diff --git a/sample.txt b/sample.txt\n--- a/sample.txt\n+++ b/sample.txt\n@@ -1,{count} +1,{count} @@\n"
        );
        for n in 1..=count {
            if changes.contains(&n) {
                patch.push_str(&format!("-line {n}\n+line {n} changed\n"));
            } else {
                patch.push_str(&format!(" line {n}\n"));
            }
        }
        patch
    }

    #[test]
    fn layout_status_body_footer_and_sidebar_boundaries() {
        let l = Layout::new(100, 20);
        assert_eq!(
            (l.body_height, l.main_width, l.sidebar_width, l.sidebar_x),
            (18, 67, 32, 68)
        );
        assert_eq!(Layout::new(89, 20).sidebar_width, 0);
        assert_eq!(Layout::new(120, 20).sidebar_width, 36);
        assert_eq!(Layout::new(300, 20).sidebar_width, 52);
        assert_eq!(file_tree_start(20, 0, 5), 0);
        assert_eq!(file_tree_start(20, 3, 5), 0);
        assert_eq!(file_tree_start(20, 4, 5), 1);
        assert_eq!(file_tree_start(20, 4, 1), 0);
        assert_eq!(move_index(0, 0, -1), 0);
        assert_eq!(move_index(2, 4, isize::MAX), 3);
    }

    #[test]
    fn word_wrap_prefers_words_code_delimiters_and_assignment() {
        for (text, first, continuation, expected) in [
            ("abcdefghi", 4, 3, vec!["abcd", "efg", "hi"]),
            ("alpha beta gamma", 12, 12, vec!["alpha beta", "gamma"]),
            ("call(alpha,beta)", 11, 11, vec!["call(alpha,", "beta)"]),
            (
                "const value = call()",
                14,
                14,
                vec!["const value =", "call()"],
            ),
            ("架abc", 3, 3, vec!["架a", "bc"]),
            ("", 3, 3, vec![""]),
        ] {
            assert_eq!(wrap_ansi(text, first, continuation), expected);
        }
        assert_eq!(wrap_widths(80, 8, "    code"), (72, 67, 13));
        assert_eq!(
            wrap_widths(21, 8, &format!("{}code", " ".repeat(40))),
            (13, 1, 20)
        );
        assert_eq!(wrap_widths(80, 8, "\tcode"), (72, 67, 13));
    }

    #[test]
    fn wrap_retains_syntax_on_continuations_and_strips_injection() {
        let rows = wrap_ansi("\x1b[31mabcdef\x1b[0m", 3, 3);
        assert_eq!(rows[0], "\x1b[31mabc");
        assert_eq!(rows[1], "\x1b[31mdef\x1b[0m");
        assert_eq!(
            plain_text("safe\x1b]52;c;ZXZpbA==\x07\x1b[2J\x1b[31m!\x1b[0m\tend"),
            "safe! end"
        );
        let rows = wrap_ansi("e\u{301}架", 1, 1);
        assert_eq!(rows.len(), 2);
        assert_eq!(plain_text(&rows[0]), "e\u{301}");
    }

    #[test]
    fn inline_background_survives_resets_and_unicode_boundaries() {
        let p = theme::catppuccin_mocha();
        let inline = inline_bg(false, DiffLineKind::Delete);
        let text = apply_inline(
            "\x1b[31m架old\x1b[0mValue",
            &[Range { start: 0, end: 11 }],
            ANSI,
            p.diff_del_bg,
            inline,
        );
        let code = ANSI.bg(inline);
        assert!(text.matches(&code).count() >= 2);
        let styled = style_cell(&text, 30, ANSI, p.diff_del_bg, p.diff_del_fg);
        assert!(styled.contains(&format!(
            "\x1b[0m{}{}{}",
            ANSI.bg(p.diff_del_bg),
            ANSI.fg(p.diff_del_fg),
            code
        )));
        assert_eq!(display_width(&styled), 30);
        assert_eq!(plain_text(&styled).trim_end(), "架oldValue");
        let unchanged = apply_inline(
            "abc",
            &[Range { start: 0, end: 1 }],
            PLAIN,
            p.diff_del_bg,
            inline,
        );
        assert_eq!(unchanged, "abc");
    }

    #[test]
    fn selected_stacked_and_split_text_are_patch_text() {
        let f = Fixture::new(PATCH);
        let mut s = State::default();
        s.selection_start = Some(f.row(&s, "old"));
        s.cursor_row = f.row(&s, "new");
        assert_eq!(selected_text(&f.snapshot, &s), ("-old\n+new".into(), 2));
        s.mode = ViewMode::Split;
        s.selection_start = None;
        s.cursor_row = f.row(&s, "new");
        assert_eq!(selected_text(&f.snapshot, &s), ("-old\n+new".into(), 2));
        s.cursor_row = f.row(&s, "keep");
        assert_eq!(selected_text(&f.snapshot, &s), (" keep".into(), 1));
        s.cursor_row = 0;
        assert_eq!(selected_text(&f.snapshot, &s).0, "sample.txt (-1 +3)");
    }

    #[test]
    fn copy_and_osc52_cannot_execute_source_sequences() {
        let f = Fixture::new(&PATCH.replace("+new", "+new\x1b]52;c;evil\x07\x1b[2J"));
        let mut s = State::default();
        s.cursor_row = s
            .view(&f.snapshot)
            .rows
            .iter()
            .position(|r| r.line.is_some_and(|l| l.kind == DiffLineKind::Add))
            .unwrap();
        let (text, _) = selected_text(&f.snapshot, &s);
        assert_eq!(text, "+new");
        let sequence = osc52(&text);
        let encoded = sequence
            .strip_prefix("\x1b]52;c;")
            .unwrap()
            .strip_suffix('\x07')
            .unwrap();
        assert_eq!(STANDARD.decode(encoded).unwrap(), b"+new");
    }

    #[test]
    fn keyboard_edges_prefixes_help_selection_and_release() {
        let mut f = Fixture::new(PATCH);
        let mut s = State::default();
        let l = Layout::new(80, 12);
        f.key(&mut s, KeyCode::Char('G'), l);
        assert_eq!(s.cursor_row, s.view(&f.snapshot).rows.len() - 1);
        f.key(&mut s, KeyCode::Char('g'), l);
        assert_eq!(s.pending, Some('g'));
        f.key(&mut s, KeyCode::Char('g'), l);
        assert_eq!(s.cursor_row, 0);
        f.key(&mut s, KeyCode::Char('V'), l);
        f.key(&mut s, KeyCode::Down, l);
        assert_eq!(s.selection_start, Some(0));
        assert_eq!(s.cursor_row, 1);
        f.key(&mut s, KeyCode::Esc, l);
        assert_eq!(s.selection_start, None);
        s.help = true;
        s.selection_start = Some(0);
        s.notice = "copied".into();
        s.pending = Some('g');
        f.key(&mut s, KeyCode::Char('j'), l);
        assert_eq!(s.cursor_row, 1);
        assert!(!s.help);
        assert_eq!(s.selection_start, Some(0));
        assert_eq!(s.pending, None);
        assert!(s.notice.is_empty());
        let release = KeyEvent::new_with_kind(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
            KeyEventKind::Release,
        );
        assert_eq!(
            handle_key(&f.snapshot, &mut f.store, &mut s, release, l).unwrap(),
            KeyAction::Continue
        );
        assert_eq!(f.key(&mut s, KeyCode::Char('y'), l), KeyAction::Copy);
        assert_eq!(f.key(&mut s, KeyCode::Char('c'), l), KeyAction::Comment);
        assert_eq!(f.key(&mut s, KeyCode::Char('q'), l), KeyAction::Quit);
    }

    #[test]
    fn layout_changes_clear_selection_not_folds_in_unfold_mode() {
        let mut f = Fixture::new(&long_patch(30, &[20]));
        let mut s = State::default();
        let l = Layout::new(80, 12);
        s.selection_start = Some(1);
        f.key(&mut s, KeyCode::Char('z'), l);
        assert_eq!(s.selection_start, Some(1));
        assert!(s.folds.is_empty());
        f.key(&mut s, KeyCode::Char('v'), l);
        assert_eq!(s.mode, ViewMode::Split);
        assert_eq!(s.selection_start, None);
        s.selection_start = Some(1);
        f.key(&mut s, KeyCode::Char('C'), l);
        assert_eq!(s.fold_mode, FoldMode::Fold);
        assert_eq!(s.selection_start, None);
        assert!(
            s.view(&f.snapshot)
                .rows
                .iter()
                .any(|r| r.kind == RowKind::Fold)
        );
        f.key(&mut s, KeyCode::Char('C'), l);
        assert!(
            s.view(&f.snapshot)
                .rows
                .iter()
                .all(|r| r.kind != RowKind::Fold)
        );
    }

    #[test]
    fn comment_selection_persists_both_directions_and_nearest_header() {
        for reverse in [false, true] {
            let mut f = Fixture::new(PATCH);
            let mut s = State::default();
            let start = f.row(&s, "new");
            let end = f.row(&s, "four");
            s.selection_start = Some(if reverse { end } else { start });
            s.cursor_row = if reverse { start } else { end };
            assert!(add_comment(&f.snapshot, &mut f.store, &s, "first\nsecond", "tester").unwrap());
            let saved = Store::at(&f.store.repo_dir).unwrap();
            assert_eq!(saved.comments.len(), 1);
            let comment = &saved.comments[0];
            assert_eq!((comment.start_line, comment.end_line), (2, 4));
            assert_eq!(comment.body, "first\nsecond");
            for row in &s.view(&f.snapshot).rows {
                if let Some(line) = row.comment_line() {
                    assert!(!comment_matches(
                        comment,
                        &f.snapshot.files[0],
                        line,
                        "other-target"
                    ));
                    if ["new", "three", "four"].contains(&line.text.as_str()) {
                        assert!(comment_matches(
                            comment,
                            &f.snapshot.files[0],
                            line,
                            &f.snapshot.review_target.target_id
                        ));
                    }
                }
            }
            s.cursor_row = 0;
            s.selection_start = None;
            assert!(
                add_comment(&f.snapshot, &mut f.store, &s, "header comment", "tester").unwrap()
            );
            assert_eq!(f.store.comments[1].start_line, 1);
        }
    }

    #[test]
    fn selected_comment_preview_and_height_match_renderer_and_mouse() {
        let mut f = Fixture::new(PATCH);
        let mut s = State::default();
        let l = Layout::new(80, 12);
        s.cursor_row = f.row(&s, "new");
        add_comment(
            &f.snapshot,
            &mut f.store,
            &s,
            "history body\nsecond line",
            "tester",
        )
        .unwrap();
        let view = s.view(&f.snapshot);
        let mut renderer = f.renderer(PLAIN);
        let rendered = renderer
            .row(&f.snapshot, &f.store, &s, &view, s.cursor_row, l.main_width)
            .unwrap();
        assert_eq!(
            rendered.len(),
            row_height(&f.snapshot, &f.store, &view, &s, s.cursor_row, l)
        );
        assert_eq!(rendered.len(), 3);
        assert!(rendered[0].contains('!'));
        assert!(rendered[1].contains("tester [exact] history body"));
        let offset = height_between(
            &f.snapshot,
            &f.store,
            &view,
            &s,
            s.scroll_row,
            s.cursor_row,
            l,
        );
        assert_eq!(
            row_at_offset(&f.snapshot, &f.store, &view, &s, offset + 2, l),
            s.cursor_row
        );
        let screen = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert!(screen.contains("history body"));
        s.cursor_row = 0;
        assert!(
            !renderer
                .frame(&f.snapshot, &f.store, &mut s, l, false)
                .unwrap()
                .contains("history body")
        );
    }

    #[test]
    fn first_change_navigation_centers_both_directions() {
        let mut f = Fixture::new(&long_patch(40, &[8, 20, 32]));
        let mut s = State::default();
        let l = Layout::new(80, 12);
        assert!(first_change(&f.snapshot, &f.store, &mut s, l));
        let changes = s.view(&f.snapshot).changes;
        assert_eq!(s.cursor_row, changes[0].start_row);
        f.key(&mut s, KeyCode::Char('n'), l);
        assert_eq!(s.cursor_row, changes[1].start_row);
        assert_eq!(s.scroll_row, s.cursor_row - 5);
        f.key(&mut s, KeyCode::Char('n'), l);
        f.key(&mut s, KeyCode::Char('p'), l);
        assert_eq!(s.cursor_row, changes[1].start_row);
        assert_eq!(s.scroll_row, s.cursor_row - 5);
        let view = s.view(&f.snapshot);
        s.cursor_row = view.rows.len() - 2;
        center(&f.snapshot, &f.store, &view, &mut s, l);
        assert_eq!(s.scroll_row, s.cursor_row - 5);
    }

    #[test]
    fn file_navigation_review_and_unreviewed_are_persistent() {
        let patch = format!("{}{}", PATCH, PATCH.replace("sample.txt", "other.txt"));
        let mut f = Fixture::new(&patch);
        let mut s = State::default();
        let l = Layout::new(100, 12);
        s.selection_start = Some(0);
        f.key(&mut s, KeyCode::Char('J'), l);
        assert_eq!(s.active_file, 1);
        assert_eq!(s.selection_start, None);
        assert_eq!(s.cursor_row, s.view(&f.snapshot).changes[0].start_row);
        f.key(&mut s, KeyCode::Char('K'), l);
        f.key(&mut s, KeyCode::Char('r'), l);
        let file = &f.snapshot.files[0];
        let saved = Store::at(&f.store.repo_dir).unwrap();
        assert!(saved.is_reviewed(
            &file.path,
            &file.patch_fingerprint,
            &f.snapshot.review_target.target_id
        ));
        f.key(&mut s, KeyCode::Char('u'), l);
        assert_eq!(s.active_file, 1);
        f.key(&mut s, KeyCode::Char('['), l);
        f.key(&mut s, KeyCode::Char('f'), l);
        assert_eq!(s.active_file, 0);
    }

    #[test]
    fn fold_toggle_requires_fold_row_and_keeps_visible_code_offset() {
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            let f = Fixture::new(&long_patch(130, &[118]));
            let l = Layout::new(80, 24);
            let mut s = State {
                mode,
                fold_mode: FoldMode::Fold,
                ..State::default()
            };
            toggle_folds(&f.snapshot, &f.store, &mut s, KeyCode::Char('z'), l);
            assert!(s.folds.is_empty());
            assert_eq!(s.cursor_row, 0);
            let view = s.view(&f.snapshot);
            let fold_row = view
                .rows
                .iter()
                .position(|r| r.kind == RowKind::Fold)
                .unwrap();
            let code_row = f.row(&s, "line 115");
            let offset = height_between(&f.snapshot, &f.store, &view, &s, 0, code_row, l);
            s.cursor_row = fold_row;
            toggle_folds(&f.snapshot, &f.store, &mut s, KeyCode::Char('z'), l);
            let updated = s.view(&f.snapshot);
            assert_eq!(
                updated.rows[s.cursor_row].comment_line().unwrap().text,
                "line 115"
            );
            assert_eq!(
                height_between(
                    &f.snapshot,
                    &f.store,
                    &updated,
                    &s,
                    s.scroll_row,
                    s.cursor_row,
                    l
                ),
                offset
            );
            let mut renderer = f.renderer(PLAIN);
            renderer
                .frame(&f.snapshot, &f.store, &mut s, l, false)
                .unwrap();
            assert_eq!(
                height_between(
                    &f.snapshot,
                    &f.store,
                    &updated,
                    &s,
                    s.scroll_row,
                    s.cursor_row,
                    l
                ),
                offset
            );
        }
    }

    #[test]
    fn all_folds_toggle_preserves_anchor_and_clears_selection() {
        let f = Fixture::new(&long_patch(130, &[118]));
        let l = Layout::new(80, 24);
        let mut s = State {
            fold_mode: FoldMode::Fold,
            selection_start: Some(0),
            ..State::default()
        };
        toggle_folds(&f.snapshot, &f.store, &mut s, KeyCode::Char('Z'), l);
        assert_eq!(s.selection_start, None);
        assert_eq!(
            s.view(&f.snapshot).rows[s.cursor_row]
                .comment_line()
                .unwrap()
                .text,
            "line 115"
        );
        assert!(
            s.view(&f.snapshot)
                .rows
                .iter()
                .filter(|r| r.kind == RowKind::Fold)
                .all(|r| r.fold_expanded)
        );
        toggle_folds(&f.snapshot, &f.store, &mut s, KeyCode::Char('Z'), l);
        assert!(
            s.view(&f.snapshot)
                .rows
                .iter()
                .any(|r| r.kind == RowKind::Fold && !r.fold_expanded)
        );
    }

    #[test]
    fn mouse_click_drag_wheel_and_sidebar_use_visual_rows() {
        let patch = format!(
            "{}{}",
            PATCH.replace("+new", &format!("+{}", "long_word ".repeat(20))),
            PATCH.replace("sample.txt", "other.txt")
        );
        let f = Fixture::new(&patch);
        let mut s = State::default();
        let l = Layout::new(100, 24);
        let mouse = |kind, x, y| MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: KeyModifiers::NONE,
        };
        let view = s.view(&f.snapshot);
        let long = view
            .rows
            .iter()
            .position(|r| {
                r.line
                    .is_some_and(|line| line.text.starts_with("long_word"))
            })
            .unwrap();
        let offset = height_between(&f.snapshot, &f.store, &view, &s, 0, long, l);
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                5,
                (offset + 2) as u16,
            ),
            l,
        );
        assert_eq!(s.cursor_row, long);
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(MouseEventKind::Drag(MouseButton::Left), 5, 2),
            l,
        );
        assert_eq!(s.selection_start, Some(long));
        assert!(s.cursor_row < long);
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(
                MouseEventKind::Down(MouseButton::Left),
                l.sidebar_x as u16,
                3,
            ),
            l,
        );
        assert_eq!(s.active_file, 1);
        assert_eq!(s.selection_start, None);
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(MouseEventKind::ScrollUp, l.sidebar_x as u16, 4),
            l,
        );
        assert_eq!(s.active_file, 0);
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(MouseEventKind::ScrollDown, 1, 4),
            l,
        );
        assert!(s.cursor_row >= long);
    }

    #[test]
    fn oversized_wrapped_line_can_scroll_without_losing_content() {
        let f = Fixture::new(&PATCH.replace("+new", &format!("+{}TAIL", "word ".repeat(250))));
        let mut s = State::default();
        let l = Layout::new(40, 8);
        let view = s.view(&f.snapshot);
        s.cursor_row = view
            .rows
            .iter()
            .position(|r| r.line.is_some_and(|l| l.text.ends_with("TAIL")))
            .unwrap();
        s.scroll_row = s.cursor_row;
        let mut renderer = f.renderer(PLAIN);
        assert!(
            !renderer
                .frame(&f.snapshot, &f.store, &mut s, l, false)
                .unwrap()
                .contains("TAIL")
        );
        while s.scroll_line
            < row_height(&f.snapshot, &f.store, &view, &s, s.cursor_row, l) - l.body_height
        {
            move_line(&f.snapshot, &f.store, &mut s, 3, true, l);
        }
        let frame = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert!(frame.contains("TAIL"));
        assert!(s.scroll_line > 0);
        move_line(&f.snapshot, &f.store, &mut s, -3, true, l);
        assert!(
            !renderer
                .frame(&f.snapshot, &f.store, &mut s, l, false)
                .unwrap()
                .contains("TAIL")
        );
    }

    #[test]
    fn every_rendered_row_matches_geometry_and_exact_terminal_width() {
        let f = Fixture::new(&PATCH.replace(
            "+new",
            &format!("+    架{}", "let value = call(alpha,beta); ".repeat(8)),
        ));
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            for width in [1, 20, 31, 32, 59, 60, 80, 100, 140] {
                let s = State {
                    mode,
                    ..State::default()
                };
                let l = Layout::new(width, 24);
                let view = s.view(&f.snapshot);
                let mut renderer = f.renderer(ANSI);
                for (index, _) in view.rows.iter().enumerate() {
                    let rows = renderer
                        .row(&f.snapshot, &f.store, &s, &view, index, l.main_width)
                        .unwrap();
                    assert_eq!(
                        rows.len(),
                        row_height(&f.snapshot, &f.store, &view, &s, index, l)
                    );
                    for row in rows {
                        assert_eq!(
                            display_width(&row),
                            l.main_width,
                            "width={width}, mode={mode:?}"
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn frame_fully_repaints_status_footer_and_static_is_escape_free() {
        let f = Fixture::new(PATCH);
        let mut s = State::default();
        let l = Layout::new(100, 12);
        let mut renderer = f.renderer(PLAIN);
        let frame = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert_eq!(frame.lines().count(), 12);
        for line in frame.lines() {
            assert_eq!(display_width(line), 100);
        }
        assert!(!frame.contains('\x1b'));
        assert!(frame.contains("files"));
        assert!(frame.contains("mode=stacked/unfold"));
        let mut renderer = f.renderer(ANSI);
        s.help = true;
        let frame = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, true)
            .unwrap();
        assert!(frame.contains("\x1b[1;1H"));
        assert!(frame.contains("\x1b[12;1H"));
        assert_eq!(frame.matches("\x1b[K").count(), 12);
        assert!(!frame.contains('\n'));
    }

    #[test]
    fn real_syntax_cache_and_inline_pair_colors_render_together() {
        let patch = "diff --git a/sample.rs b/sample.rs\n--- a/sample.rs\n+++ b/sample.rs\n@@ -1 +1 @@\n-fn old_name() { let x = 1; }\n+fn new_name() { let x = 2; }\n";
        let f = Fixture::new(patch);
        std::fs::create_dir_all(&f.snapshot.repository.root_path).unwrap();
        std::fs::write(
            f.snapshot.repository.root_path.join("sample.rs"),
            "fn new_name() { let x = 2; }\n",
        )
        .unwrap();
        let s = State {
            mode: ViewMode::Split,
            ..State::default()
        };
        let view = s.view(&f.snapshot);
        let index = view.changes[0].start_row;
        let mut renderer = f.renderer(ANSI);
        let row = renderer
            .row(&f.snapshot, &f.store, &s, &view, index, 120)
            .unwrap()
            .join("");
        assert!(row.contains(&ANSI.fg(renderer.palette.syntax_keyword)));
        assert!(row.contains(&ANSI.bg(inline_bg(false, DiffLineKind::Add))));
        assert!(row.contains(&ANSI.bg(inline_bg(false, DiffLineKind::Delete))));
        assert!(plain_text(&row).contains("fn new_name()"));
    }

    #[test]
    fn randomized_wrapping_terminates_and_matches_plain_geometry() {
        let alphabet = ["a", "b", " ", "  ", "架", "\u{301}", ",", "=", ")", "\t"];
        let mut random = 0x9e3779b97f4a7c15_u64;
        for _ in 0..1000 {
            let mut text = String::new();
            for _ in 0..50 {
                random ^= random << 13;
                random ^= random >> 7;
                random ^= random << 17;
                text.push_str(alphabet[(random as usize) % alphabet.len()]);
            }
            let width = (random as usize % 20) + 2;
            let plain = wrap_ansi(&text, width, width - 1);
            let highlighted = wrap_ansi(&format!("\x1b[31m{text}\x1b[0m"), width, width - 1);
            assert_eq!(plain.len(), highlighted.len());
            for (a, b) in plain.iter().zip(highlighted) {
                assert_eq!(plain_text(a), plain_text(&b));
            }
        }
    }
    #[test]
    #[ignore = "Requires a controlling PTY; run under a terminal cleanup harness"]
    fn terminal_panic_probe() {
        assert!(io::stdin().is_terminal() && io::stdout().is_terminal());
        let result = std::panic::catch_unwind(|| {
            let mut guard = TerminalGuard::enter().unwrap();
            guard.editor_keys(true).unwrap();
            panic!("terminal cleanup probe");
        });
        assert!(result.is_err());
        assert!(!terminal::is_raw_mode_enabled().unwrap());
    }
    #[test]
    #[cfg(unix)]
    fn clipboard_pipe_transfers_real_input_and_can_be_interrupted() {
        let interrupted = AtomicBool::new(false);
        let text = "copied text\n".repeat(20_000);
        assert!(piped_command(&["/bin/cat"], &text, &interrupted));
        std::thread::scope(|scope| {
            scope.spawn(|| {
                std::thread::sleep(Duration::from_millis(30));
                interrupted.store(true, Ordering::Relaxed);
            });
            assert!(!piped_command(&["/bin/sleep", "10"], &text, &interrupted));
        });
    }

    #[test]
    fn tab_indented_source_keeps_real_syntax_highlighting() {
        let patch = "diff --git a/sample.rs b/sample.rs\n--- /dev/null\n+++ b/sample.rs\n@@ -0,0 +1 @@\n+\tfn main() {}\n";
        let f = Fixture::new(patch);
        std::fs::create_dir_all(&f.snapshot.repository.root_path).unwrap();
        std::fs::write(
            f.snapshot.repository.root_path.join("sample.rs"),
            "\tfn main() {}\n",
        )
        .unwrap();
        let s = State::default();
        let view = s.view(&f.snapshot);
        let mut renderer = f.renderer(ANSI);
        let rows = renderer
            .row(
                &f.snapshot,
                &f.store,
                &s,
                &view,
                view.changes[0].start_row,
                80,
            )
            .unwrap();
        assert!(rows[0].contains(&ANSI.fg(renderer.palette.syntax_keyword)));
        assert_eq!(display_width(&rows[0]), 80);
    }
}
