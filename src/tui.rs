use crate::{
    Error, Result,
    diff::{DiffFile, DiffLine, DiffLineKind, DiffSnapshot},
    inline_diff::{self, Range},
    store::{Comment, Store},
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
    collections::HashMap,
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
#[path = "tui_screen.rs"]
mod screen;

const CURSOR_GUTTER: usize = 2;
const HELP: &[&str] = &[
    "HELP  Esc / ? close",
    "",
    "Tab          Open / close file navigation",
    "j/k arrows   Move cursor; select a file in FILES",
    "Enter        Open selected file / view comments",
    "J/K          Next / previous file, keeping its position",
    "n/p          Next / previous change",
    "PgUp/PgDn    Page; scroll comments in COMMENTS",
    "G/gg         Last / first line",
    "c            Comment on cursor / selected code",
    "V / Esc      Select range / clear selection",
    "y            Copy cursor / selected diff",
    "v            Stacked / split layout",
    "C / z / Z    Fold mode / current fold / all folds",
    "r / u        Mark reviewed / first unreviewed file",
    "q / Ctrl+C   Quit",
];
// Request enhanced keys where supported; the input decoder also accepts legacy
// modifyOtherKeys and Shift+Enter forms. Reset resources on leaving the editor.
const ENABLE_EDITOR_KEYS: &str = "\x1b[>1u\x1b[>4;1f\x1b[>4;2m";
const DISABLE_EDITOR_KEYS: &str = "\x1b[<u\x1b[>4;0m\x1b[>4f";

#[derive(Debug, Clone, Copy)]
struct Layout {
    width: usize,
    height: usize,
    body_height: usize,
    main_x: usize,
    main_width: usize,
    sidebar_width: usize,
    sidebar_overlay: bool,
    dock_y: usize,
    dock_height: usize,
}
impl Layout {
    fn new(width: usize, height: usize) -> Self {
        // Honor actual small terminals rather than writing outside their screen.
        let width = width.max(1);
        let height = height.max(3);
        Self {
            width,
            height,
            body_height: height - 2,
            main_x: 0,
            main_width: width,
            sidebar_width: 0,
            sidebar_overlay: false,
            dock_y: height - 1,
            dock_height: 0,
        }
    }
    fn with_panels(self, state: &State) -> Self {
        let mut layout = Self::new(self.width, self.height);
        if state.sidebar_open {
            if self.width >= 112 {
                layout.sidebar_width = (self.width / 4).clamp(26, 34);
                layout.main_x = layout.sidebar_width + 1;
                layout.main_width -= layout.main_x;
            } else if state.focus == Focus::Files {
                layout.sidebar_width = self.width;
                layout.sidebar_overlay = true;
            }
        }
        if state.comment_open && self.height >= 7 && !layout.sidebar_overlay {
            layout.dock_height = (self.height / 3).clamp(3, 8);
            layout.dock_y -= layout.dock_height;
            layout.body_height -= layout.dock_height;
        }
        layout
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Code,
    Files,
    Comments,
    Editor,
}

#[derive(Debug, Clone, Copy)]
struct ReadingPosition {
    cursor: (usize, usize),
    scroll: (usize, usize),
    mode: ViewMode,
    fold_mode: FoldMode,
    width: usize,
}
impl ReadingPosition {
    fn capture(state: &State, layout: Layout) -> Self {
        Self {
            cursor: (state.cursor_row, state.cursor_line),
            scroll: (state.scroll_row, state.scroll_line),
            mode: state.mode,
            fold_mode: state.fold_mode,
            width: layout.main_width,
        }
    }
    fn restore(self, state: &mut State, layout: Layout) {
        (state.cursor_row, state.cursor_line) = self.cursor;
        (state.scroll_row, state.scroll_line) = self.scroll;
        state.mode = self.mode;
        state.fold_mode = self.fold_mode;
        if self.width != layout.main_width {
            state.cursor_line = 0;
            state.scroll_line = 0;
        }
    }
}

#[derive(Debug)]
struct State {
    active_file: usize,
    cursor_row: usize,
    // Paging and wheel navigation can stop within a wrapped code row.
    cursor_line: usize,
    scroll_row: usize,
    scroll_line: usize,
    mode: ViewMode,
    fold_mode: FoldMode,
    selection_start: Option<usize>,
    notice: String,
    help: bool,
    help_scroll: usize,
    pending: Option<char>,
    folds: Vec<FoldEntry>,
    focus: Focus,
    sidebar_open: bool,
    file_cursor: usize,
    comment_open: bool,
    comment_scroll: usize,
    dock_origin: Option<ReadingPosition>,
    positions: HashMap<usize, ReadingPosition>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            active_file: 0,
            cursor_row: 0,
            cursor_line: 0,
            scroll_row: 0,
            scroll_line: 0,
            mode: ViewMode::Stacked,
            fold_mode: FoldMode::Unfold,
            selection_start: None,
            notice: String::new(),
            help: false,
            help_scroll: 0,
            pending: None,
            folds: Vec::new(),
            focus: Focus::Code,
            sidebar_open: false,
            file_cursor: 0,
            comment_open: false,
            comment_scroll: 0,
            dock_origin: None,
            positions: HashMap::new(),
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
        self.selection_start.is_some_and(|start| {
            (start.min(self.cursor_row)..=start.max(self.cursor_row)).contains(&row)
        })
    }
    fn clear_selection(&mut self) {
        self.selection_start = None;
        self.notice.clear();
        self.help = false;
        self.pending = None;
    }
    fn open_comments(&mut self, focus: Focus, layout: Layout) {
        if !self.comment_open {
            self.dock_origin = Some(ReadingPosition::capture(self, layout));
        }
        self.comment_open = true;
        self.comment_scroll = 0;
        self.focus = focus;
    }
    fn close_comments(&mut self, layout: Layout) {
        if let Some(origin) = self.dock_origin.take()
            && origin.cursor == (self.cursor_row, self.cursor_line)
            && origin.mode == self.mode
            && origin.fold_mode == self.fold_mode
            && origin.width == layout.main_width
        {
            (self.scroll_row, self.scroll_line) = origin.scroll;
        }
        self.comment_open = false;
        self.comment_scroll = 0;
        self.focus = Focus::Code;
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
        screen: screen::Screen::default(),
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
        // Pace wheel bursts, not isolated inputs or keyboard/editor actions.
        #[cfg(unix)]
        let scroll_deadline = std::time::Instant::now() + Duration::from_millis(16);
        let layout = Layout::terminal().with_panels(&state);
        let mut frame = renderer.frame(snapshot, store, &mut state, layout, true)?;
        if let Some(input) = &editor {
            frame.push_str(&input.draw(layout, renderer.ansi, renderer.palette));
            renderer
                .screen
                .invalidate_rows(layout.dock_y + 1..layout.height - 1);
        }
        // End synchronized output only after the dock editor has been painted.
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
        if matches!(event, Event::Resize(_, _)) {
            renderer.screen.invalidate();
        }
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
                    state.close_comments(layout);
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
            Event::Mouse(mouse) => {
                #[cfg(unix)]
                let repeat = if matches!(
                    mouse.kind,
                    MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
                ) {
                    input.coalesce_scroll(mouse, scroll_deadline)
                } else {
                    1
                };
                #[cfg(not(unix))]
                let repeat = 1;
                handle_mouse(snapshot, store, &mut state, mouse, repeat, layout);
            }
            Event::Resize(_, _) => {
                state.cursor_line = 0;
                state.scroll_line = 0;
                state.selection_start = None;
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
            let _ = io::stdout().write_all(b"\x1b[?2026l\x1b[r\x1b[0m");
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
    let layout = layout.with_panels(state);
    if key.code == KeyCode::Esc {
        if state.help {
            state.help = false;
        } else if state.focus == Focus::Files {
            state.sidebar_open = false;
            state.focus = Focus::Code;
        } else if state.focus == Focus::Comments
            || (state.selection_start.is_none() && state.comment_open)
        {
            state.close_comments(layout);
        } else if state.selection_start.is_none() {
            state.sidebar_open = false;
        }
        state.clear_selection();
        return Ok(KeyAction::Continue);
    }
    if state.help && key.code != KeyCode::Char('?') {
        let delta = match key.code {
            KeyCode::Char('j') | KeyCode::Down => 1,
            KeyCode::Char('k') | KeyCode::Up => -1,
            KeyCode::PageDown => layout.height.saturating_sub(3) as isize,
            KeyCode::PageUp => -(layout.height.saturating_sub(3) as isize),
            _ => 0,
        };
        if delta != 0 {
            state.help_scroll = state
                .help_scroll
                .saturating_add_signed(delta)
                .min(HELP.len().saturating_sub(layout.height - 2));
            return Ok(KeyAction::Continue);
        }
        state.help = false;
        state.pending = None;
        state.notice.clear();
        return Ok(KeyAction::Continue);
    }
    if key.code != KeyCode::Char('y') {
        state.notice.clear();
    }
    let pending = state.pending.take();
    if key.code == KeyCode::Tab {
        state.selection_start = None;
        if state.focus == Focus::Files {
            state.sidebar_open = false;
            state.focus = Focus::Code;
        } else {
            state.sidebar_open = true;
            state.file_cursor = state.active_file;
            state.focus = Focus::Files;
        }
        return Ok(KeyAction::Continue);
    }
    if state.focus == Focus::Files {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => {
                state.file_cursor = move_index(state.file_cursor, snapshot.files.len(), 1)
            }
            KeyCode::Char('k') | KeyCode::Up => {
                state.file_cursor = move_index(state.file_cursor, snapshot.files.len(), -1)
            }
            KeyCode::PageDown => {
                state.file_cursor = move_index(
                    state.file_cursor,
                    snapshot.files.len(),
                    layout.height.saturating_sub(3) as isize,
                )
            }
            KeyCode::PageUp => {
                state.file_cursor = move_index(
                    state.file_cursor,
                    snapshot.files.len(),
                    -(layout.height.saturating_sub(3) as isize),
                )
            }
            KeyCode::Home => state.file_cursor = 0,
            KeyCode::End | KeyCode::Char('G') => state.file_cursor = snapshot.files.len() - 1,
            KeyCode::Enter => open_file(snapshot, store, state, state.file_cursor, layout),
            KeyCode::Char('?') => state.help = true,
            _ => {}
        }
        return Ok(KeyAction::Continue);
    }
    if state.focus == Focus::Comments {
        let delta = match key.code {
            KeyCode::Char('j') | KeyCode::Down => 1,
            KeyCode::Char('k') | KeyCode::Up => -1,
            KeyCode::PageDown => layout.dock_height.saturating_sub(2).max(1) as isize,
            KeyCode::PageUp => -(layout.dock_height.saturating_sub(2).max(1) as isize),
            _ => 0,
        };
        if delta != 0 {
            state.comment_scroll = state.comment_scroll.saturating_add_signed(delta);
            return Ok(KeyAction::Continue);
        }
        match key.code {
            KeyCode::Enter => state.focus = Focus::Code,
            KeyCode::Home | KeyCode::Char('g') => state.comment_scroll = 0,
            KeyCode::End | KeyCode::Char('G') => state.comment_scroll = usize::MAX,
            KeyCode::Char('c' | '?' | 'J' | 'K') => {}
            _ => return Ok(KeyAction::Continue),
        }
        if !matches!(key.code, KeyCode::Char('c' | '?' | 'J' | 'K')) {
            return Ok(KeyAction::Continue);
        }
    }
    match key.code {
        KeyCode::Char('g') if pending != Some('g') => state.pending = Some('g'),
        KeyCode::Char('g') | KeyCode::Home => {
            state.cursor_row = 0;
            state.cursor_line = 0;
        }
        KeyCode::Char('G') | KeyCode::End => {
            state.cursor_row = state.view(snapshot).rows.len().saturating_sub(1);
            state.cursor_line = usize::MAX;
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
        KeyCode::Char('j') | KeyCode::Down => move_line(snapshot, state, 1),
        KeyCode::Char('k') | KeyCode::Up => move_line(snapshot, state, -1),
        KeyCode::PageDown | KeyCode::PageUp => {
            let page = layout.body_height.saturating_sub(1).max(1) as isize;
            move_visual(
                snapshot,
                store,
                state,
                if key.code == KeyCode::PageDown {
                    page
                } else {
                    -page
                },
                layout,
            );
        }
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
                state.cursor_line = 0;
                center(snapshot, store, &view, state, layout);
            }
        }
        KeyCode::Char('C') | KeyCode::Char('z') | KeyCode::Char('Z') => {
            toggle_folds(snapshot, store, state, key.code, layout)
        }
        KeyCode::Char('v') => {
            let view = state.view(snapshot);
            let anchor = capture_anchor(snapshot, store, &view, state, layout);
            state.selection_start = None;
            state.mode = if state.mode == ViewMode::Stacked {
                ViewMode::Split
            } else {
                ViewMode::Stacked
            };
            let updated = state.view(snapshot);
            restore_anchor(snapshot, store, &updated, state, anchor, layout);
        }
        KeyCode::Char('V') => {
            state.selection_start = if state.selection_start.is_some() {
                None
            } else {
                Some(state.cursor_row)
            };
        }
        KeyCode::Char('y') => return Ok(KeyAction::Copy),
        KeyCode::Char('c') | KeyCode::Enter => {
            let view = state.view(snapshot);
            if comment_anchor(&snapshot.files[state.active_file], &view, state).is_some() {
                if layout.height < 7 || layout.main_width < 20 {
                    state.notice = "enlarge terminal to open comments".into();
                } else if key.code == KeyCode::Char('c') {
                    state.open_comments(Focus::Editor, layout);
                    return Ok(KeyAction::Comment);
                } else {
                    state.open_comments(Focus::Comments, layout);
                }
            } else {
                state.notice = "select a code line to comment".into();
            }
        }
        KeyCode::Char('?') => state.help = !state.help,
        KeyCode::Char('u') => {
            if let Some(index) = snapshot.files.iter().position(|file| {
                !store.is_reviewed(
                    &file.path,
                    &file.patch_fingerprint,
                    &snapshot.review_target.target_id,
                )
            }) {
                open_file(snapshot, store, state, index, layout);
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
    ensure_visible(snapshot, store, &view, state, layout.with_panels(state));
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
    let next = move_index(state.active_file, snapshot.files.len(), delta);
    if next == state.active_file {
        return;
    }
    open_file(snapshot, store, state, next, layout);
}
fn open_file(
    snapshot: &DiffSnapshot,
    store: &Store,
    state: &mut State,
    index: usize,
    layout: Layout,
) {
    state.close_comments(layout);
    state.focus = Focus::Code;
    if layout.sidebar_overlay {
        state.sidebar_open = false;
    }
    let layout = layout.with_panels(state);
    state.selection_start = None;
    state.file_cursor = index;
    if index == state.active_file {
        return;
    }
    state
        .positions
        .insert(state.active_file, ReadingPosition::capture(state, layout));
    state.active_file = index;
    if let Some(position) = state.positions.get(&index).copied() {
        position.restore(state, layout);
        let view = state.view(snapshot);
        ensure_visible(snapshot, store, &view, state, layout);
    } else {
        first_change(snapshot, store, state, layout);
    }
}
fn first_change(snapshot: &DiffSnapshot, store: &Store, state: &mut State, layout: Layout) -> bool {
    state.cursor_row = 0;
    state.cursor_line = 0;
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
fn move_line(snapshot: &DiffSnapshot, state: &mut State, delta: isize) {
    let next = move_index(state.cursor_row, state.view(snapshot).rows.len(), delta);
    if next != state.cursor_row {
        state.cursor_row = next;
        state.cursor_line = 0;
    }
}
fn move_visual(
    snapshot: &DiffSnapshot,
    store: &Store,
    state: &mut State,
    delta: isize,
    layout: Layout,
) {
    let view = state.view(snapshot);
    (state.cursor_row, state.cursor_line) = visual_position(
        view.rows.len(),
        (state.cursor_row, state.cursor_line),
        delta,
        |row| row_height(snapshot, store, &view, state, row, layout),
    );
    ensure_visible(snapshot, store, &view, state, layout);
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
    repeat: u16,
    layout: Layout,
) {
    let layout = layout.with_panels(state);
    // Ignore motion-only events and chrome outside the interactive body.
    if state.help
        || mouse.column as usize >= layout.width
        || mouse.row < 1
        || mouse.row as usize >= layout.height - 1
        || matches!(mouse.kind, MouseEventKind::Moved | MouseEventKind::Up(_))
    {
        return;
    }
    state.pending = None;
    state.notice.clear();
    let sidebar = (mouse.column as usize) < layout.sidebar_width;
    let dock = !sidebar && layout.dock_height > 0 && mouse.row as usize >= layout.dock_y;
    let delta = match mouse.kind {
        MouseEventKind::ScrollUp => -3 * repeat as isize,
        MouseEventKind::ScrollDown => 3 * repeat as isize,
        _ => 0,
    };
    if delta != 0 {
        if sidebar {
            state.focus = Focus::Files;
            state.file_cursor = move_index(state.file_cursor, snapshot.files.len(), delta);
        } else if dock {
            state.focus = Focus::Comments;
            state.comment_scroll = state.comment_scroll.saturating_add_signed(delta);
        } else {
            state.focus = Focus::Code;
            move_visual(snapshot, store, state, delta, layout);
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
            file_tree_start(snapshot.files.len(), state.file_cursor, layout.height - 2) + y - 2;
        if index < snapshot.files.len() {
            open_file(snapshot, store, state, index, layout);
        }
    } else if dock {
        state.focus = Focus::Comments;
    } else if mouse.column as usize >= layout.main_x {
        state.focus = Focus::Code;
        let view = state.view(snapshot);
        if view.rows.is_empty() {
            return;
        }
        let (row, line) = row_at_offset(snapshot, store, &view, state, y - 1, layout);
        if drag {
            state.selection_start.get_or_insert(state.cursor_row);
        } else {
            state.selection_start = None;
        }
        state.cursor_row = row;
        state.cursor_line = line;
        state.comment_scroll = 0;
        ensure_visible(snapshot, store, &view, state, layout);
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
    _snapshot: &DiffSnapshot,
    _store: &Store,
    view: &FileView<'_>,
    _state: &State,
    index: usize,
    layout: Layout,
) -> usize {
    let row = &view.rows[index];
    let digits = view.line_number_width;
    let width = layout.main_width.saturating_sub(CURSOR_GUTTER);
    match row.kind {
        RowKind::StackedCode | RowKind::FileMeta => row
            .line
            .map_or(1, |line| line_height(line, width, digits + 5)),
        RowKind::SplitCode if width < 32 => row
            .right
            .or(row.left)
            .map_or(1, |line| line_height(line, width, digits + 5)),
        RowKind::SplitCode => {
            let left_width = (width - 3) / 2;
            let right_width = width - 3 - left_width;
            row.left
                .map_or(1, |line| line_height(line, left_width, digits + 4))
                .max(
                    row.right
                        .map_or(1, |line| line_height(line, right_width, digits + 4)),
                )
        }
        _ => 1,
    }
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
// Cursor navigation, viewport following and mouse hit testing share visual geometry.
fn visual_position(
    len: usize,
    position: (usize, usize),
    delta: isize,
    height: impl Fn(usize) -> usize,
) -> (usize, usize) {
    if len == 0 {
        return (0, 0);
    }
    let (mut row, mut line) = position;
    row = row.min(len - 1);
    line = line.min(height(row).saturating_sub(1));
    let mut remaining = delta.unsigned_abs();
    while remaining > 0 {
        let available = if delta > 0 {
            height(row) - 1 - line
        } else {
            line
        };
        if remaining <= available {
            line = if delta > 0 {
                line + remaining
            } else {
                line - remaining
            };
            break;
        }
        if delta > 0 && row + 1 == len {
            line = height(row) - 1;
            break;
        }
        if delta < 0 && row == 0 {
            line = 0;
            break;
        }
        remaining -= available + 1;
        if delta > 0 {
            row += 1;
            line = 0;
        } else {
            row -= 1;
            line = height(row) - 1;
        }
    }
    (row, line)
}
fn center(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &mut State,
    layout: Layout,
) {
    (state.scroll_row, state.scroll_line) = visual_position(
        view.rows.len(),
        (state.cursor_row, state.cursor_line),
        -((layout.body_height / 2) as isize),
        |row| row_height(snapshot, store, view, state, row, layout),
    );
    ensure_visible(snapshot, store, view, state, layout);
}
fn ensure_visible(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &mut State,
    layout: Layout,
) {
    if view.rows.is_empty() {
        state.cursor_row = 0;
        state.cursor_line = 0;
        state.scroll_row = 0;
        state.scroll_line = 0;
        return;
    }
    state.cursor_row = state.cursor_row.min(view.rows.len() - 1);
    let cursor_height = row_height(snapshot, store, view, state, state.cursor_row, layout);
    state.cursor_line = state.cursor_line.min(cursor_height - 1);
    let height = |row| row_height(snapshot, store, view, state, row, layout);
    let scroll = visual_position(
        view.rows.len(),
        (state.scroll_row, state.scroll_line),
        0,
        height,
    );
    // Keep ordinary rows whole; oversized rows follow the cursor's wrapped position.
    let (first, last) = if cursor_height <= layout.body_height {
        (0, cursor_height - 1)
    } else {
        (state.cursor_line, state.cursor_line)
    };
    let top = (state.cursor_row, first);
    let bottom = (state.cursor_row, last);
    let end = visual_position(
        view.rows.len(),
        scroll,
        (layout.body_height - 1) as isize,
        height,
    );
    let scroll = if top < scroll {
        top
    } else if bottom > end {
        visual_position(
            view.rows.len(),
            bottom,
            -((layout.body_height - 1) as isize),
            height,
        )
    } else {
        scroll
    };
    (state.scroll_row, state.scroll_line) = scroll;
}
fn row_at_offset(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &State,
    offset: usize,
    layout: Layout,
) -> (usize, usize) {
    visual_position(
        view.rows.len(),
        (state.scroll_row, state.scroll_line),
        offset as isize,
        |row| row_height(snapshot, store, view, state, row, layout),
    )
}
fn capture_anchor<'a>(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'a>,
    state: &State,
    layout: Layout,
) -> Option<(VisualRow<'a>, usize)> {
    let row = view.rows.get(state.cursor_row)?.clone();
    let offset = height_between(
        snapshot,
        store,
        view,
        state,
        state.scroll_row,
        state.cursor_row,
        layout,
    )
    .saturating_sub(state.scroll_line);
    Some((row, offset))
}
fn restore_anchor(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &mut State,
    anchor: Option<(VisualRow<'_>, usize)>,
    layout: Layout,
) {
    state.cursor_line = 0;
    if let Some((original, offset)) = anchor {
        let line = original
            .comment_line()
            .or_else(|| original.fold_lines.first());
        let same_line = |candidate: &DiffLine| {
            line.is_some_and(|line| line.stable_line_id == candidate.stable_line_id)
        };
        let index = view
            .rows
            .iter()
            .position(|row| {
                if original.kind == RowKind::Fold && row.kind == RowKind::Fold {
                    row.fold_id == original.fold_id
                } else if original.kind == RowKind::Fold && row.kind != RowKind::Fold
                    || original.comment_line().is_some()
                {
                    [row.line, row.left, row.right]
                        .into_iter()
                        .flatten()
                        .any(same_line)
                } else {
                    row.kind == original.kind && row.hunk_index == original.hunk_index
                }
            })
            .or_else(|| {
                view.rows.iter().position(|row| {
                    row.kind == RowKind::Fold && row.fold_lines.iter().any(same_line)
                })
            });
        if let Some(index) = index {
            state.cursor_row = index;
            (state.scroll_row, state.scroll_line) =
                visual_position(view.rows.len(), (index, 0), -(offset as isize), |row| {
                    row_height(snapshot, store, view, state, row, layout)
                });
        }
    }
    ensure_visible(snapshot, store, view, state, layout);
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
    let target = view.rows.get(state.cursor_row).and_then(|row| row.fold_id);
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
    restore_anchor(snapshot, store, &updated, state, anchor, layout);
}

fn cursor_position(view: &FileView<'_>, state: &State) -> String {
    view.rows
        .get(state.cursor_row)
        .map_or_else(String::new, |row| {
            row.comment_line()
                .and_then(|line| {
                    line_number(line).map(|number| {
                        format!(
                            "{}:{number}",
                            if line.kind == DiffLineKind::Delete {
                                "old"
                            } else {
                                "new"
                            },
                        )
                    })
                })
                .unwrap_or_else(|| {
                    match row.kind {
                        RowKind::FileHeader => "file",
                        RowKind::HunkHeader => "hunk",
                        RowKind::Fold => "fold",
                        _ => "meta",
                    }
                    .into()
                })
        })
}
fn cursor_label(view: &FileView<'_>, state: &State) -> String {
    let position = cursor_position(view, state);
    if let Some(start) = state.selection_start {
        format!(
            "SELECT {} rows  {position}",
            start.abs_diff(state.cursor_row) + 1
        )
    } else {
        format!("CODE {position}")
    }
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
        let number = line_number(line)?;
        Some((
            line,
            row.hunk_index
                .and_then(|i| file.hunks.get(i))
                .map_or("", |h| h.header.as_str()),
            number,
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
    None
}
fn comment_target_label(file: &DiffFile, view: &FileView<'_>, state: &State) -> String {
    let Some((line, _, end)) = comment_anchor(file, view, state) else {
        return "no code line".into();
    };
    let start = line_number(line).unwrap_or(0);
    let side = if line.kind == DiffLineKind::Delete {
        "old"
    } else {
        "new"
    };
    if start == end {
        format!("{side}:{start}")
    } else {
        format!("{side}:{start}-{end}")
    }
}
fn comment_rows(
    snapshot: &DiffSnapshot,
    store: &Store,
    view: &FileView<'_>,
    state: &State,
    width: usize,
) -> Vec<String> {
    let mut rows = Vec::new();
    let file = &snapshot.files[state.active_file];
    if let Some(row) = view.rows.get(state.cursor_row) {
        for comment in store
            .comments
            .iter()
            .filter(|c| row_has_comment(row, c, file, &snapshot.review_target.target_id))
        {
            if !rows.is_empty() {
                rows.push(String::new());
            }
            rows.push(plain_text(&format!(
                "{} [{}]",
                comment.author,
                comment.match_status.label()
            )));
            for text in comment.body.split('\n') {
                rows.extend(wrap_ansi(&plain_text(text), width, width));
            }
        }
    }
    if rows.is_empty() {
        rows.push("No comments here. Press c to add one.".into());
    }
    rows
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
    screen: screen::Screen,
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
        let layout = layout.with_panels(state);
        let view = state.view(snapshot);
        // A picker must not move the code cursor or load the candidate file's syntax.
        if !layout.sidebar_overlay && !state.help {
            ensure_visible(snapshot, store, &view, state, layout);
        }
        let file = &snapshot.files[state.active_file];
        let target = &snapshot.review_target.target_id;
        let reviewed = snapshot
            .files
            .iter()
            .filter(|file| store.is_reviewed(&file.path, &file.patch_fingerprint, target))
            .count();
        let spec = if snapshot.review_target.normalized_spec.is_empty() {
            "working tree"
        } else {
            &snapshot.review_target.normalized_spec
        };
        let right = plain_text(&format!(
            "  {spec}  {}/{}  {reviewed}/{} reviewed ",
            state.mode.label(),
            state.fold_mode.label(),
            snapshot.files.len()
        ));
        let path = plain_text(&format!(" {}", display_path(file)));
        let status = if layout.width >= 80 && display_width(&right) < layout.width / 2 {
            format!(
                "{}{right}",
                fit_cell(&path, layout.width - display_width(&right))
            )
        } else {
            format!("{path}  {right}")
        };
        let mut lines = vec![self.panel(&status, layout.width, false)];
        let mut diff_rows = Vec::new();
        if !layout.sidebar_overlay && !state.help {
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
        }
        let comments = if layout.dock_height > 0 && state.focus != Focus::Editor {
            comment_rows(
                snapshot,
                store,
                &view,
                state,
                layout.main_width.saturating_sub(2).max(1),
            )
        } else {
            Vec::new()
        };
        state.comment_scroll = state.comment_scroll.min(
            comments
                .len()
                .saturating_sub(layout.dock_height.saturating_sub(1)),
        );
        let anchor = if state.focus == Focus::Editor {
            comment_target_label(file, &view, state)
        } else {
            cursor_position(&view, state)
        };
        let tree_start =
            file_tree_start(snapshot.files.len(), state.file_cursor, layout.height - 2);
        for y in 0..layout.height - 2 {
            if state.help {
                lines.push(self.panel(
                    HELP.get(state.help_scroll + y).copied().unwrap_or(""),
                    layout.width,
                    false,
                ));
                continue;
            }
            let mut line = String::new();
            if layout.sidebar_width > 0 {
                line.push_str(&self.file_row(
                    snapshot,
                    store,
                    state,
                    y,
                    tree_start,
                    layout.sidebar_width,
                ));
                if layout.sidebar_overlay {
                    lines.push(line);
                    continue;
                }
                line.push_str(&self.panel("│", 1, false));
            }
            let main = if y < layout.body_height {
                diff_rows.get(y).cloned().unwrap_or_else(|| {
                    style_cell(
                        "",
                        layout.main_width,
                        self.ansi,
                        self.palette.bg_default,
                        self.palette.fg_default,
                    )
                })
            } else if y == layout.body_height {
                let title = if state.focus == Focus::Editor {
                    "COMMENT"
                } else {
                    "COMMENTS"
                };
                style_cell(
                    &plain_text(&format!(" {title}  {anchor}  {}", file.path)),
                    layout.main_width,
                    self.ansi,
                    self.palette.bg_selected,
                    self.palette.fg_accent,
                )
            } else {
                let text = comments
                    .get(state.comment_scroll + y - layout.body_height - 1)
                    .map_or("", String::as_str);
                style_cell(
                    &format!(" {text}"),
                    layout.main_width,
                    self.ansi,
                    self.palette.bg_panel,
                    self.palette.fg_default,
                )
            };
            line.push_str(&main);
            lines.push(line);
        }
        let footer = if state.help {
            "HELP  j/k scroll".into()
        } else {
            match state.focus {
                Focus::Files => "FILES  j/k select  Enter open  ? help".into(),
                Focus::Comments => "COMMENTS  j/k scroll  c comment  Enter code".into(),
                Focus::Editor if layout.dock_height < 2 || layout.main_width < 3 => {
                    "COMMENT  Enlarge terminal".into()
                }
                Focus::Editor => "COMMENT  Enter save  Shift+Enter newline".into(),
                Focus::Code if layout.width < 80 => {
                    format!("{}  Tab files  c comment", cursor_label(&view, state))
                }
                Focus::Code => format!(
                    "{}  Tab files  Enter notes  c comment  V {}  y copy",
                    cursor_label(&view, state),
                    if state.selection_start.is_some() {
                        "clear"
                    } else {
                        "select"
                    }
                ),
            }
        };
        let footer = if state.notice.is_empty() {
            footer
        } else {
            format!("{}  {footer}", state.notice)
        };
        let hint = if layout.width < 16 {
            if state.focus == Focus::Code && !state.help {
                "?"
            } else {
                "Esc"
            }
        } else if state.help || matches!(state.focus, Focus::Files | Focus::Comments) {
            " Esc close"
        } else if state.focus == Focus::Editor {
            " Esc cancel"
        } else {
            " ? help"
        };
        let footer = format!(
            "{}{hint}",
            fit_cell(
                &plain_text(&footer),
                layout.width.saturating_sub(display_width(hint))
            )
        );
        lines.push(self.panel(&footer, layout.width, false));
        if interactive {
            Ok(self
                .screen
                .draw(lines, layout.width, 1..layout.body_height + 1))
        } else {
            Ok(lines.join("\n"))
        }
    }

    fn file_row(
        &self,
        snapshot: &DiffSnapshot,
        store: &Store,
        state: &State,
        y: usize,
        start: usize,
        width: usize,
    ) -> String {
        if y == 0 {
            return self.panel(
                &format!(" FILES  {}/{}", state.file_cursor + 1, snapshot.files.len()),
                width,
                false,
            );
        }
        let index = start + y - 1;
        let Some(file) = snapshot.files.get(index) else {
            return self.panel("", width, false);
        };
        let target = &snapshot.review_target.target_id;
        let reviewed = store.is_reviewed(&file.path, &file.patch_fingerprint, target);
        let count = store.comment_count(&file.path, target);
        let badge = if count > 0 {
            format!(" !{count}")
        } else {
            String::new()
        };
        let focused = state.focus == Focus::Files && index == state.file_cursor;
        let marker = if focused {
            ">"
        } else if index == state.active_file {
            "*"
        } else {
            " "
        };
        let path = fit_cell(
            &plain_text(&file.path),
            width.saturating_sub(6 + display_width(&badge)),
        );
        self.panel(
            &format!(
                "{marker}{} {} {path}{badge}",
                file.status.label(),
                if reviewed { "x" } else { "." }
            ),
            width,
            focused,
        )
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
        let focused = index == state.cursor_row && state.focus == Focus::Code;
        let selected = index == state.cursor_row || state.selected(index);
        let gutter_width = width.min(CURSOR_GUTTER);
        let width = width - gutter_width;
        let file = &snapshot.files[state.active_file];
        let target = &snapshot.review_target.target_id;
        let rows = match row.kind {
            RowKind::FileHeader => {
                let stats = file_stats(view);
                let count = store.comment_count(&file.path, target);
                vec![style_cell(
                    &format!(
                        "  {}  {stats}  {count} comment{}",
                        file.status.label(),
                        if count == 1 { "" } else { "s" }
                    ),
                    width,
                    self.ansi,
                    if selected {
                        self.palette.bg_selected
                    } else {
                        self.palette.bg_panel
                    },
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
                view.line_number_width,
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
                view.line_number_width,
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
                    view.line_number_width,
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
                    view.line_number_width,
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
        Ok(rows
            .into_iter()
            .enumerate()
            .map(|(line, row)| {
                let cursor = focused && line == state.cursor_line;
                let gutter = style_cell(
                    if cursor {
                        "> "
                    } else if state.selected(index) {
                        "| "
                    } else {
                        "  "
                    },
                    gutter_width,
                    self.ansi,
                    self.palette.bg_panel,
                    if cursor {
                        self.palette.fg_accent
                    } else {
                        self.palette.fg_muted
                    },
                );
                format!("{gutter}{row}")
            })
            .collect())
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
        digits: usize,
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
                screen: screen::Screen::default(),
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
            (l.body_height, l.main_width, l.sidebar_width, l.main_x),
            (18, 100, 0, 0)
        );
        let mut s = State {
            sidebar_open: true,
            ..State::default()
        };
        assert_eq!(Layout::new(89, 20).with_panels(&s).sidebar_width, 0);
        let wide = Layout::new(120, 20).with_panels(&s);
        assert_eq!(
            (wide.sidebar_width, wide.main_x, wide.main_width),
            (30, 31, 89)
        );
        assert_eq!(Layout::new(300, 20).with_panels(&s).sidebar_width, 34);
        s.focus = Focus::Files;
        let narrow = l.with_panels(&s);
        assert!(narrow.sidebar_overlay);
        assert_eq!(
            (narrow.sidebar_width, narrow.main_width, narrow.main_x),
            (100, 100, 0)
        );
        s.focus = Focus::Comments;
        s.comment_open = true;
        let dock = l.with_panels(&s);
        assert_eq!(
            (dock.body_height, dock.dock_y, dock.dock_height),
            (12, 13, 6)
        );
        assert_eq!(file_tree_start(20, 0, 5), 0);
        assert_eq!(file_tree_start(20, 3, 5), 0);
        assert_eq!(file_tree_start(20, 4, 5), 1);
        assert_eq!(file_tree_start(20, 4, 1), 0);
        assert_eq!(move_index(0, 0, -1), 0);
        assert_eq!(move_index(2, 4, isize::MAX), 3);
    }

    #[test]
    fn file_picker_selects_without_opening_and_returns_to_code() {
        let patch = format!(
            "{}{}",
            PATCH,
            PATCH
                .replace("sample.txt", "other.txt")
                .replace("+new", "+candidate body")
        );
        let mut f = Fixture::new(&patch);
        let mut s = State::default();
        let l = Layout::new(120, 24);
        s.cursor_row = f.row(&s, "three");
        let cursor = s.cursor_row;
        f.key(&mut s, KeyCode::Tab, l);
        f.key(&mut s, KeyCode::Down, l);
        assert_eq!((s.active_file, s.file_cursor, s.cursor_row), (0, 1, cursor));
        let screen = f
            .renderer(PLAIN)
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert!(screen.contains("FILES"));
        assert!(!screen.contains("candidate body"));
        assert_eq!(
            screen.lines().filter(|line| line.starts_with('>')).count(),
            1
        );
        f.key(&mut s, KeyCode::Enter, l);
        assert_eq!((s.active_file, s.focus), (1, Focus::Code));
        assert!(s.sidebar_open);
        f.key(&mut s, KeyCode::Esc, l);
        assert!(!s.sidebar_open);
        let small = Layout::new(80, 24);
        f.key(&mut s, KeyCode::Tab, small);
        assert!(small.with_panels(&s).sidebar_overlay);
        f.key(&mut s, KeyCode::Up, small);
        assert_eq!(s.active_file, 1);
        f.key(&mut s, KeyCode::Enter, small);
        assert_eq!(s.active_file, 0);
        assert_eq!(s.cursor_row, cursor);
        assert!(!s.sidebar_open);
    }

    #[test]
    fn returning_to_a_file_restores_cursor_scroll_layout_and_folds() {
        let patch = long_patch(160, &[40, 120]);
        let mut f = Fixture::new(&format!(
            "{patch}{}",
            patch.replace("sample.txt", "other.txt")
        ));
        let mut s = State {
            mode: ViewMode::Split,
            fold_mode: FoldMode::Fold,
            ..State::default()
        };
        let l = Layout::new(140, 20);
        f.key(&mut s, KeyCode::Char('Z'), l);
        s.cursor_row = f.row(&s, "line 70");
        let view = s.view(&f.snapshot);
        center(&f.snapshot, &f.store, &view, &mut s, l);
        let expected = ReadingPosition::capture(&s, l);
        s.selection_start = Some(s.cursor_row - 1);
        f.key(&mut s, KeyCode::Char('J'), l);
        assert_eq!(s.selection_start, None);
        f.key(&mut s, KeyCode::Char('v'), l);
        f.key(&mut s, KeyCode::Char('C'), l);
        f.key(&mut s, KeyCode::End, l);
        let other = ReadingPosition::capture(&s, l);
        f.key(&mut s, KeyCode::Char('K'), l);
        assert_eq!((s.cursor_row, s.cursor_line), expected.cursor);
        assert_eq!((s.scroll_row, s.scroll_line), expected.scroll);
        assert_eq!((s.mode, s.fold_mode), (expected.mode, expected.fold_mode));
        assert_eq!(
            s.view(&f.snapshot).rows[s.cursor_row]
                .comment_line()
                .unwrap()
                .text,
            "line 70"
        );
        f.key(&mut s, KeyCode::Char('J'), l);
        assert_eq!((s.cursor_row, s.cursor_line), other.cursor);
        assert_eq!((s.scroll_row, s.scroll_line), other.scroll);
        assert_eq!((s.mode, s.fold_mode), (other.mode, other.fold_mode));
        f.key(&mut s, KeyCode::Char('K'), Layout::new(80, 12));
        assert_eq!(s.cursor_row, expected.cursor.0);
        assert_eq!(s.cursor_line, 0);
        assert!(f.store.comments.is_empty());
    }

    #[test]
    fn closing_comments_restores_the_viewport_and_keeps_the_anchor() {
        let mut f = Fixture::new(&long_patch(100, &[80]));
        let l = Layout::new(100, 20);
        let mut s = State::default();
        s.cursor_row = f.row(&s, "line 80 changed");
        s.scroll_row = s.cursor_row - (l.body_height - 1);
        let original = ReadingPosition::capture(&s, l);
        f.key(&mut s, KeyCode::Enter, l);
        assert_eq!(s.focus, Focus::Comments);
        f.renderer(PLAIN)
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert!(s.scroll_row > original.scroll.0);
        f.key(&mut s, KeyCode::End, l);
        assert_eq!(s.cursor_row, original.cursor.0);
        f.key(&mut s, KeyCode::Esc, l);
        assert_eq!((s.scroll_row, s.scroll_line), original.scroll);
        s.selection_start = Some(s.cursor_row - 2);
        assert_eq!(f.key(&mut s, KeyCode::Char('c'), l), KeyAction::Comment);
        assert_eq!(s.focus, Focus::Editor);
        let view = s.view(&f.snapshot);
        assert!(comment_target_label(&f.snapshot.files[0], &view, &s).contains('-'));
        assert!(s.selection_start.is_some());
        assert_eq!(s.cursor_row, original.cursor.0);
    }

    #[test]
    fn panels_and_dock_frames_fit_narrow_short_and_wide_terminals() {
        let mut f = Fixture::new(PATCH);
        for width in [1, 19, 40, 60, 80, 100, 112, 160] {
            for height in [3, 6, 7, 12, 24] {
                for focus in [Focus::Code, Focus::Files, Focus::Comments, Focus::Editor] {
                    let mut s = State {
                        sidebar_open: true,
                        comment_open: true,
                        focus,
                        ..State::default()
                    };
                    s.cursor_row = f.row(&s, "new");
                    let base = Layout::new(width, height);
                    let l = base.with_panels(&s);
                    assert_eq!(l.body_height + l.dock_height + 2, l.height);
                    assert_eq!(l.dock_y, l.body_height + 1);
                    assert_eq!(l.main_x + l.main_width, l.width);
                    let frame = f
                        .renderer(PLAIN)
                        .frame(&f.snapshot, &f.store, &mut s, base, false)
                        .unwrap();
                    assert_eq!(frame.lines().count(), height);
                    assert!(
                        frame.lines().all(|line| display_width(line) == width),
                        "{width}x{height}, {focus:?}"
                    );
                    assert!(!frame.contains('\x1b'));
                    if width >= 19 {
                        let hint = match focus {
                            Focus::Code => "? help",
                            Focus::Editor => "Esc cancel",
                            _ => "Esc close",
                        };
                        assert!(
                            frame.lines().last().unwrap().ends_with(hint),
                            "{width}x{height}: {hint}"
                        );
                    }
                }
            }
        }
        let mut s = State::default();
        s.cursor_row = f.row(&s, "new");
        assert_eq!(
            f.key(&mut s, KeyCode::Char('c'), Layout::new(19, 6)),
            KeyAction::Continue
        );
        assert!(!s.comment_open);
        assert!(s.notice.contains("enlarge"));
        s.help = true;
        f.key(&mut s, KeyCode::PageDown, Layout::new(80, 8));
        let screen = f
            .renderer(PLAIN)
            .frame(&f.snapshot, &f.store, &mut s, Layout::new(80, 8), false)
            .unwrap();
        assert!(s.help_scroll > 0);
        assert!(screen.contains("HELP  j/k scroll"));
    }

    #[test]
    fn visual_positions_match_flat_screen_coordinates() {
        let heights = [1, 4, 2, 10];
        let positions: Vec<_> = heights
            .iter()
            .enumerate()
            .flat_map(|(row, &height)| (0..height).map(move |line| (row, line)))
            .collect();
        for (index, &position) in positions.iter().enumerate() {
            for delta in [isize::MIN, -8, -1, 0, 1, 8, isize::MAX] {
                assert_eq!(
                    visual_position(heights.len(), position, delta, |row| heights[row]),
                    positions[move_index(index, positions.len(), delta)],
                );
            }
        }
        assert_eq!(
            visual_position(0, (10, 10), 1, |_| panic!("empty view")),
            (0, 0)
        );
    }

    #[test]
    fn cursor_moves_before_viewport_and_does_not_scroll_on_redraw() {
        let mut f = Fixture::new(&long_patch(60, &[30]));
        let l = Layout::new(80, 12);
        let mut s = State::default();
        f.key(&mut s, KeyCode::Down, l);
        assert_eq!(s.cursor_row, 1);
        assert_eq!((s.scroll_row, s.scroll_line), (0, 0));
        move_visual(&f.snapshot, &f.store, &mut s, 3, l);
        assert_eq!(s.cursor_row, 4);
        assert_eq!((s.scroll_row, s.scroll_line), (0, 0));
        for _ in 0..6 {
            f.key(&mut s, KeyCode::Char('j'), l);
        }
        assert_eq!(s.cursor_row, 10);
        assert_eq!((s.scroll_row, s.scroll_line), (1, 0));
        f.key(&mut s, KeyCode::Char('k'), l);
        assert_eq!((s.scroll_row, s.scroll_line), (1, 0));
        let view = s.view(&f.snapshot);
        s.cursor_row = view.rows.len() - 2;
        center(&f.snapshot, &f.store, &view, &mut s, l);
        let scroll = (s.scroll_row, s.scroll_line);
        let cursor = s.cursor_row;
        f.renderer(PLAIN)
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert_eq!((s.scroll_row, s.scroll_line), scroll);
        f.key(&mut s, KeyCode::Char('V'), l);
        assert_eq!((s.scroll_row, s.scroll_line), scroll);
        f.key(&mut s, KeyCode::Char('J'), l);
        assert_eq!(s.cursor_row, cursor);
        assert!(s.selection_start.is_some());
    }

    #[test]
    fn page_navigation_uses_screen_height_and_wrapped_positions() {
        let mut f = Fixture::new(&long_patch(100, &[80]));
        for height in [8, 12, 24] {
            let l = Layout::new(80, height);
            let mut s = State {
                cursor_row: 1,
                ..State::default()
            };
            f.key(&mut s, KeyCode::PageDown, l);
            assert_eq!(s.cursor_row, l.body_height);
            f.key(&mut s, KeyCode::PageUp, l);
            assert_eq!((s.cursor_row, s.cursor_line), (1, 0));
        }
        let mut f = Fixture::new(&PATCH.replace("+new", &format!("+{}TAIL", "word ".repeat(250))));
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            let l = Layout::new(40, 8);
            let mut s = State {
                mode,
                ..State::default()
            };
            s.cursor_row = s
                .view(&f.snapshot)
                .rows
                .iter()
                .position(|row| {
                    row.comment_line()
                        .is_some_and(|line| line.text.ends_with("TAIL"))
                })
                .unwrap();
            let row = s.cursor_row;
            f.key(&mut s, KeyCode::PageDown, l);
            assert_eq!((s.cursor_row, s.cursor_line), (row, l.body_height - 1));
            f.key(&mut s, KeyCode::PageDown, l);
            assert_eq!(
                (s.cursor_row, s.cursor_line),
                (row, 2 * (l.body_height - 1))
            );
            assert!(s.scroll_line > 0);
            let frame = f
                .renderer(PLAIN)
                .frame(&f.snapshot, &f.store, &mut s, l, false)
                .unwrap();
            assert_eq!(
                frame.lines().filter(|line| line.starts_with("> ")).count(),
                1
            );
            f.key(&mut s, KeyCode::PageUp, l);
            f.key(&mut s, KeyCode::PageUp, l);
            assert_eq!((s.cursor_row, s.cursor_line), (row, 0));
        }
    }

    #[test]
    fn cursor_and_selection_have_distinct_markers_without_color() {
        let mut f = Fixture::new(PATCH);
        let l = Layout::new(100, 12);
        let mut s = State::default();
        s.cursor_row = f.row(&s, "new");
        let start = s.cursor_row;
        assert!(!s.selected(start));
        for ansi in [PLAIN, ANSI] {
            let mut renderer = f.renderer(ansi);
            let view = s.view(&f.snapshot);
            let row = renderer
                .row(&f.snapshot, &f.store, &s, &view, start, l.main_width)
                .unwrap();
            assert!(plain_text(&row[0]).starts_with("> "));
        }
        f.key(&mut s, KeyCode::Char('V'), l);
        f.key(&mut s, KeyCode::Char('j'), l);
        let view = s.view(&f.snapshot);
        let mut renderer = f.renderer(PLAIN);
        assert!(
            renderer
                .row(&f.snapshot, &f.store, &s, &view, start, l.main_width)
                .unwrap()[0]
                .starts_with("| ")
        );
        assert!(
            renderer
                .row(&f.snapshot, &f.store, &s, &view, s.cursor_row, l.main_width)
                .unwrap()[0]
                .starts_with("> ")
        );
        assert!(
            renderer
                .frame(&f.snapshot, &f.store, &mut s, l, false)
                .unwrap()
                .contains("SELECT 2 rows")
        );
        assert_eq!(selected_text(&f.snapshot, &s), ("+new\n+three".into(), 2));
        f.key(&mut s, KeyCode::Char('V'), l);
        assert_eq!(s.selection_start, None);
        assert!(
            renderer
                .frame(&f.snapshot, &f.store, &mut s, l, false)
                .unwrap()
                .contains("CODE new:3")
        );
        f.key(&mut s, KeyCode::Home, l);
        assert_eq!(f.key(&mut s, KeyCode::Char('c'), l), KeyAction::Continue);
        assert_eq!(s.notice, "select a code line to comment");
        assert!(f.store.comments.is_empty());
        let view = s.view(&f.snapshot);
        assert!(
            renderer
                .row(&f.snapshot, &f.store, &s, &view, 0, l.main_width)
                .unwrap()[0]
                .starts_with("> ")
        );
    }

    #[test]
    fn layout_and_fold_changes_keep_the_cursor_source_position() {
        let mut f = Fixture::new(&long_patch(100, &[10, 30, 70]));
        let l = Layout::new(100, 20);
        for text in ["line 30 changed", "line 65"] {
            let mut s = State::default();
            s.cursor_row = f.row(&s, text);
            let id = s.view(&f.snapshot).rows[s.cursor_row]
                .comment_line()
                .unwrap()
                .stable_line_id
                .clone();
            for _ in 0..2 {
                f.key(&mut s, KeyCode::Char('v'), l);
                assert_eq!(
                    s.view(&f.snapshot).rows[s.cursor_row]
                        .comment_line()
                        .unwrap()
                        .stable_line_id,
                    id
                );
            }
        }
        let mut s = State::default();
        s.cursor_row = f.row(&s, "line 50");
        f.key(&mut s, KeyCode::Char('C'), l);
        let row = s.view(&f.snapshot).rows[s.cursor_row].clone();
        assert_eq!(row.kind, RowKind::Fold);
        assert!(row.fold_lines.iter().any(|line| line.text == "line 50"));
        let id = row.fold_id;
        f.key(&mut s, KeyCode::Char('v'), l);
        assert_eq!(s.view(&f.snapshot).rows[s.cursor_row].fold_id, id);
        f.key(&mut s, KeyCode::Char('z'), l);
        assert_eq!(s.view(&f.snapshot).rows[s.cursor_row].fold_id, id);
        s.cursor_row = f.row(&s, "line 70 changed");
        f.key(&mut s, KeyCode::Char('Z'), l);
        assert_eq!(
            s.view(&f.snapshot).rows[s.cursor_row]
                .comment_line()
                .unwrap()
                .text,
            "line 70 changed"
        );
    }

    #[test]
    fn selection_only_previews_comments_at_the_cursor() {
        let mut f = Fixture::new(PATCH);
        let mut s = State::default();
        let l = Layout::new(80, 12);
        s.cursor_row = f.row(&s, "new");
        add_comment(&f.snapshot, &mut f.store, &s, "preview", "tester").unwrap();
        let start = s.cursor_row;
        f.key(&mut s, KeyCode::Char('V'), l);
        f.key(&mut s, KeyCode::Char('j'), l);
        let view = s.view(&f.snapshot);
        assert!(s.selected(start));
        assert_eq!(row_height(&f.snapshot, &f.store, &view, &s, start, l), 1);
        let row = f
            .renderer(PLAIN)
            .row(&f.snapshot, &f.store, &s, &view, start, l.main_width)
            .unwrap();
        assert_eq!(row.len(), 1);
        assert!(!row[0].contains("preview"));
        f.key(&mut s, KeyCode::Enter, l);
        let frame = f
            .renderer(PLAIN)
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert!(frame.contains("COMMENTS  new:3"));
        assert!(!frame.contains("preview"));
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
        f.key(&mut s, KeyCode::Char('x'), l);
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
    fn comment_selection_persists_both_directions_without_nearest_line_fallback() {
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
                !add_comment(&f.snapshot, &mut f.store, &s, "header comment", "tester").unwrap()
            );
            assert_eq!(f.store.comments.len(), 1);
        }
    }

    #[test]
    fn comments_only_render_in_the_dock_and_do_not_change_code_geometry() {
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
        assert_eq!(rendered.len(), 1);
        assert!(rendered[0].contains('!'));
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
            row_at_offset(&f.snapshot, &f.store, &view, &s, offset + 1, l),
            (s.cursor_row + 1, 0)
        );
        let screen = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert!(!screen.contains("history body"));
        let cursor = s.cursor_row;
        f.key(&mut s, KeyCode::Enter, l);
        assert_eq!(s.focus, Focus::Comments);
        let screen = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        let dock = l.with_panels(&s);
        assert_eq!(s.cursor_row, cursor);
        assert!(
            screen
                .lines()
                .nth(dock.dock_y)
                .unwrap()
                .contains("COMMENTS  new:2")
        );
        assert!(
            screen
                .lines()
                .nth(dock.dock_y + 1)
                .unwrap()
                .contains("tester [exact]")
        );
        assert!(screen.contains("history body"));
        f.key(&mut s, KeyCode::Char('j'), l);
        assert_eq!(s.cursor_row, cursor);
        f.key(&mut s, KeyCode::Esc, l);
        assert!(!s.comment_open);
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
    fn fold_toggle_keeps_cursor_on_fold_and_collapses_from_its_context() {
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            let mut f = Fixture::new(&long_patch(130, &[118]));
            let l = Layout::new(80, 24);
            let mut s = State {
                mode,
                fold_mode: FoldMode::Fold,
                ..State::default()
            };
            f.key(&mut s, KeyCode::Char('z'), l);
            assert!(s.folds.is_empty());
            assert_eq!(s.cursor_row, 0);
            let view = s.view(&f.snapshot);
            let fold_row = view
                .rows
                .iter()
                .position(|r| r.kind == RowKind::Fold)
                .unwrap();
            let id = view.rows[fold_row].fold_id;
            s.cursor_row = fold_row;
            f.key(&mut s, KeyCode::Char('z'), l);
            let updated = s.view(&f.snapshot);
            assert_eq!(s.cursor_row, fold_row);
            assert_eq!(updated.rows[s.cursor_row].fold_id, id);
            assert!(updated.rows[s.cursor_row].fold_expanded);
            f.key(&mut s, KeyCode::Char('j'), l);
            assert_eq!(
                s.view(&f.snapshot).rows[s.cursor_row]
                    .comment_line()
                    .unwrap()
                    .text,
                "line 1"
            );
            f.key(&mut s, KeyCode::Char('z'), l);
            assert_eq!(s.cursor_row, fold_row);
            assert!(!s.view(&f.snapshot).rows[s.cursor_row].fold_expanded);
            f.key(&mut s, KeyCode::Char('C'), l);
            assert_eq!(
                s.view(&f.snapshot).rows[s.cursor_row]
                    .comment_line()
                    .unwrap()
                    .text,
                "line 1"
            );
            f.key(&mut s, KeyCode::Char('C'), l);
            assert_eq!(s.view(&f.snapshot).rows[s.cursor_row].fold_id, id);
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
        assert_eq!(s.cursor_row, 0);
        assert_eq!(
            s.view(&f.snapshot).rows[s.cursor_row].kind,
            RowKind::FileHeader
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
        let mut f = Fixture::new(&patch);
        let mut s = State::default();
        let l = Layout::new(140, 24);
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
            1,
            l,
        );
        assert_eq!(s.cursor_row, long);
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(MouseEventKind::Drag(MouseButton::Left), 5, 2),
            1,
            l,
        );
        assert_eq!(s.selection_start, Some(long));
        assert!(s.cursor_row < long);
        s.sidebar_open = true;
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(MouseEventKind::Down(MouseButton::Left), 1, 3),
            1,
            l,
        );
        assert_eq!(s.active_file, 1);
        assert_eq!(s.selection_start, None);
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(MouseEventKind::ScrollUp, 1, 4),
            1,
            l,
        );
        assert_eq!(s.active_file, 1);
        assert_eq!(s.file_cursor, 0);
        f.key(&mut s, KeyCode::Enter, l);
        let x = l.with_panels(&s).main_x as u16 + 1;
        handle_mouse(
            &f.snapshot,
            &f.store,
            &mut s,
            mouse(MouseEventKind::ScrollDown, x, 4),
            1,
            l,
        );
        assert!(s.cursor_row >= long);
    }

    #[test]
    fn batched_wheel_selects_files_without_opening_them() {
        let patch: String = (0..8)
            .map(|n| PATCH.replace("sample.txt", &format!("file{n}.txt")))
            .collect();
        let f = Fixture::new(&patch);
        for (kind, start, count, expected) in [
            (MouseEventKind::ScrollDown, 0, 2, 6),
            (MouseEventKind::ScrollDown, 0, 256, 7),
            (MouseEventKind::ScrollUp, 7, 2, 1),
            (MouseEventKind::ScrollUp, 7, 256, 0),
        ] {
            let mut s = State {
                active_file: 2,
                cursor_row: 3,
                scroll_row: 1,
                sidebar_open: true,
                file_cursor: start,
                ..State::default()
            };
            let mouse = MouseEvent {
                kind,
                column: 2,
                row: 4,
                modifiers: KeyModifiers::NONE,
            };
            handle_mouse(
                &f.snapshot,
                &f.store,
                &mut s,
                mouse,
                count,
                Layout::new(140, 24),
            );
            assert_eq!(s.file_cursor, expected);
            assert_eq!(s.focus, Focus::Files);
            assert_eq!((s.active_file, s.cursor_row, s.scroll_row), (2, 3, 1));
        }
    }

    #[test]
    fn batched_wheel_matches_single_events_across_wrapping_and_folds() {
        let patch = long_patch(90, &[10, 50, 80])
            .replace(" line 40\n", &format!(" {}\n", "wide word ".repeat(180)));
        let f = Fixture::new(&patch);
        let l = Layout::new(80, 16);
        for mode in [ViewMode::Stacked, ViewMode::Split] {
            for fold_mode in [FoldMode::Fold, FoldMode::Unfold] {
                for kind in [MouseEventKind::ScrollUp, MouseEventKind::ScrollDown] {
                    for count in [1, 8, 48, 256] {
                        let initial = || {
                            let mut state = State {
                                mode,
                                fold_mode,
                                ..State::default()
                            };
                            if kind == MouseEventKind::ScrollUp {
                                state.cursor_row = state.view(&f.snapshot).rows.len() - 1;
                            }
                            state.selection_start = Some(state.cursor_row);
                            state
                        };
                        let mut single = initial();
                        let mut batch = initial();
                        let view = single.view(&f.snapshot);
                        ensure_visible(&f.snapshot, &f.store, &view, &mut single, l);
                        ensure_visible(&f.snapshot, &f.store, &view, &mut batch, l);
                        let mouse = MouseEvent {
                            kind,
                            column: 5,
                            row: 5,
                            modifiers: KeyModifiers::NONE,
                        };
                        for _ in 0..count {
                            handle_mouse(&f.snapshot, &f.store, &mut single, mouse, 1, l);
                        }
                        handle_mouse(&f.snapshot, &f.store, &mut batch, mouse, count, l);
                        let position = |s: &State| {
                            (
                                s.cursor_row,
                                s.cursor_line,
                                s.scroll_row,
                                s.scroll_line,
                                s.selection_start,
                            )
                        };
                        assert_eq!(
                            position(&single),
                            position(&batch),
                            "{mode:?} {fold_mode:?} {kind:?} {count}"
                        );
                    }
                }
            }
        }
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
        let cursor = s.cursor_row;
        let height = row_height(&f.snapshot, &f.store, &view, &s, cursor, l);
        move_visual(&f.snapshot, &f.store, &mut s, (height - 1) as isize, l);
        assert_eq!(s.cursor_row, cursor);
        let frame = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, false)
            .unwrap();
        assert!(frame.contains("TAIL"));
        assert!(s.scroll_line > 0);
        let scroll = (s.scroll_row, s.scroll_line);
        move_visual(&f.snapshot, &f.store, &mut s, -3, l);
        assert_eq!((s.scroll_row, s.scroll_line), scroll);
        move_visual(&f.snapshot, &f.store, &mut s, -(l.body_height as isize), l);
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
    fn repeated_navigation_only_paints_changed_rows_and_scrolls_the_body() {
        let mut f = Fixture::new(&long_patch(120, &[60]));
        let mut s = State::default();
        let l = Layout::new(100, 24);
        let mut renderer = f.renderer(ANSI);
        renderer
            .frame(&f.snapshot, &f.store, &mut s, l, true)
            .unwrap();
        let mut scrolled_up = false;
        let mut scrolled_down = false;
        for key in std::iter::repeat_n('j', 80).chain(std::iter::repeat_n('k', 80)) {
            f.key(&mut s, KeyCode::Char(key), l);
            let frame = renderer
                .frame(&f.snapshot, &f.store, &mut s, l, true)
                .unwrap();
            assert!(
                frame.matches(";1H").count() <= 4,
                "Navigation repainted unchanged rows"
            );
            assert!(frame.contains("\x1b[1;1H"));
            assert!(frame.contains("\x1b[24;1H"));
            scrolled_up |= frame.contains("\x1b[1S");
            scrolled_down |= frame.contains("\x1b[1T");
        }
        assert!(scrolled_up && scrolled_down);
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
        assert!(frame.contains("stacked/unfold"));
        let mut renderer = f.renderer(ANSI);
        s.help = true;
        let frame = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, true)
            .unwrap();
        assert!(frame.contains("\x1b[1;1H"));
        assert!(frame.contains("\x1b[12;1H"));
        assert_eq!(frame.matches(";1H").count(), 12);
        assert!(!frame.contains("\x1b[K"));
        assert!(!frame.contains('\n'));
        let unchanged = renderer
            .frame(&f.snapshot, &f.store, &mut s, l, true)
            .unwrap();
        assert_eq!(unchanged.matches(";1H").count(), 2);
        assert!(unchanged.contains("\x1b[1;1H"));
        assert!(unchanged.contains("\x1b[12;1H"));
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
