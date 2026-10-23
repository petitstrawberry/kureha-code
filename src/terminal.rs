//! Integrated PTY terminal. Call `poll()` from the application's regular window
//! sync, and mount `view()` in the workbench. Startup, I/O and child cleanup all
//! run on a worker; UI calls never wait for the shell or join its thread.
//!
//! `open(folder)` starts a fresh shell in that folder. Hiding the panel may keep
//! the session alive; `close()` or dropping the last Terminal releases it.
mod backend;
mod screen;
#[cfg(test)]
mod tests;

use scarlet_ui::{
    Event, FontStack, KeyCode, KeyEvent, State, TextGrid, TextGridBuffer, TextGridCursor, View,
    ViewExt, generate_state_id,
};
use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

pub const CELL_WIDTH: f32 = 8.0;
pub const CELL_HEIGHT: f32 = 17.0;
const DEFAULT_SIZE: (u16, u16) = (80, 16);
const INPUT_LIMIT: usize = 64 * 1024;
const SCROLLBACK_LIMIT: usize = 1000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TerminalStatus {
    Idle,
    Starting,
    Running,
    Exited(Option<i32>),
    Closed,
    Error(String),
}
impl std::fmt::Display for TerminalStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Idle => f.write_str("Terminal closed"),
            Self::Starting => f.write_str("Starting shell…"),
            Self::Running => f.write_str("Shell running"),
            Self::Exited(Some(code)) => write!(f, "Shell exited ({code})"),
            Self::Exited(None) => f.write_str("Shell exited"),
            Self::Closed => f.write_str("Terminal closed"),
            Self::Error(error) => write!(f, "Terminal: {error}"),
        }
    }
}

#[derive(Default)]
struct Inbox {
    input: VecDeque<u8>,
    size: Option<(u16, u16)>,
    scroll: isize,
}
struct Snapshot {
    grid: TextGridBuffer,
    cursor: TextGridCursor,
    application_cursor: bool,
    bracketed_paste: bool,
    status: TerminalStatus,
}
struct Session {
    cancel: Arc<AtomicBool>,
    inbox: Arc<Mutex<Inbox>>,
    output: Arc<Mutex<Option<Snapshot>>>,
}
impl Drop for Session {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Release);
    }
}
struct Shared {
    // Only UI-facing methods access this lock. The worker never takes it.
    session: Mutex<Option<Session>>,
    size: Mutex<(u16, u16)>,
    grid: State<TextGridBuffer>,
    cursor: State<TextGridCursor>,
    status: State<TerminalStatus>,
    focused: State<bool>,
    application_cursor: AtomicBool,
    bracketed_paste: AtomicBool,
    generation: AtomicU64,
}

#[derive(Clone)]
pub struct Terminal {
    shared: Arc<Shared>,
}
impl Default for Terminal {
    fn default() -> Self {
        Self::new()
    }
}
impl Terminal {
    /// Construct an idle terminal. `open` is deliberately separate so opening
    /// the application does not implicitly start a process.
    pub fn new() -> Self {
        Self {
            shared: Arc::new(Shared {
                session: Mutex::new(None),
                size: Mutex::new(DEFAULT_SIZE),
                grid: State::new(generate_state_id(), screen::blank(DEFAULT_SIZE)),
                cursor: State::new(generate_state_id(), TextGridCursor::new(0, 0)),
                status: State::new(generate_state_id(), TerminalStatus::Idle),
                focused: State::new(generate_state_id(), false),
                application_cursor: AtomicBool::new(false),
                bracketed_paste: AtomicBool::new(false),
                generation: AtomicU64::new(0),
            }),
        }
    }

    /// Queue shell startup and return immediately. Reopening cancels the old
    /// session; its worker closes/reaps independently of the new session.
    pub fn open(&self, cwd: impl AsRef<Path>) {
        self.close();
        let size = *self.shared.size.lock().unwrap();
        self.shared.grid.set(screen::blank(size));
        self.shared.status.set(TerminalStatus::Starting);
        self.shared
            .application_cursor
            .store(false, Ordering::Relaxed);
        self.shared.bracketed_paste.store(false, Ordering::Relaxed);
        let session = Session {
            cancel: Arc::new(AtomicBool::new(false)),
            inbox: Arc::new(Mutex::new(Inbox::default())),
            output: Arc::new(Mutex::new(None)),
        };
        let cancel = session.cancel.clone();
        let inbox = session.inbox.clone();
        let output = session.output.clone();
        let cwd = cwd.as_ref().to_path_buf();
        *self.shared.session.lock().unwrap() = Some(session);
        if let Err(error) = thread::Builder::new()
            .name("terminal-pty".into())
            .spawn(move || worker(cwd, size, cancel, inbox, output))
        {
            self.shared.session.lock().unwrap().take();
            self.shared
                .status
                .set(TerminalStatus::Error(error.to_string()));
        }
    }

    /// Publish at most one latest snapshot, with no blocking receive and no
    /// unbounded output queue. Returns whether the parent should rebuild chrome.
    pub fn poll(&self) -> bool {
        let snapshot = {
            let session = self.shared.session.lock().unwrap();
            let Some(session) = session.as_ref() else {
                return false;
            };
            let Ok(mut output) = session.output.try_lock() else {
                return false;
            };
            output.take()
        };
        let Some(snapshot) = snapshot else {
            return false;
        };
        self.shared.grid.set(snapshot.grid);
        self.shared.cursor.set(snapshot.cursor);
        self.shared.status.set(snapshot.status);
        self.shared
            .application_cursor
            .store(snapshot.application_cursor, Ordering::Relaxed);
        self.shared
            .bracketed_paste
            .store(snapshot.bracketed_paste, Ordering::Relaxed);
        true
    }
    pub fn status(&self) -> TerminalStatus {
        self.shared.status.get()
    }
    pub fn is_running(&self) -> bool {
        matches!(
            self.status(),
            TerminalStatus::Starting | TerminalStatus::Running
        )
    }
    pub fn grid(&self) -> TextGridBuffer {
        self.shared.grid.get()
    }
    pub fn is_focused(&self) -> bool {
        self.focused()
    }
    pub fn focused(&self) -> bool {
        self.shared.focused.get()
    }
    pub fn focus(&self) {
        self.shared.focused.set(true);
    }
    pub fn blur(&self) {
        self.shared.focused.set(false);
    }

    /// Cancel without waiting. The worker sends hangup/kill and reaps the child.
    pub fn generation(&self) -> u64 {
        self.shared.generation.load(Ordering::Relaxed)
    }
    pub fn close(&self) {
        self.shared.generation.fetch_add(1, Ordering::Relaxed);
        self.shared.session.lock().unwrap().take();
        self.shared.status.set(TerminalStatus::Closed);
        self.shared.focused.set(false);
    }
    /// Resize is coalesced and bounded, including when a pane is collapsed.
    pub fn resize(&self, columns: u16, rows: u16) {
        let size = (columns.clamp(2, 300), rows.clamp(1, 120));
        let mut previous = self.shared.size.lock().unwrap();
        if *previous == size {
            return;
        }
        if self.with_inbox(|inbox| {
            inbox.size = Some(size);
            true
        }) || !self.is_running()
        {
            *previous = size;
        }
    }
    pub fn resize_pixels(&self, width: f32, height: f32) {
        if width.is_finite() && height.is_finite() && width > 0.0 && height > 0.0 {
            self.resize((width / CELL_WIDTH) as u16, (height / CELL_HEIGHT) as u16);
        }
    }
    fn with_inbox(&self, f: impl FnOnce(&mut Inbox) -> bool) -> bool {
        let session = self.shared.session.lock().unwrap();
        let Some(session) = session.as_ref() else {
            return false;
        };
        let Ok(mut inbox) = session.inbox.try_lock() else {
            return false;
        };
        f(&mut inbox)
    }
    /// Atomic bounded enqueue; false means closed, busy, or input capacity full.
    /// Callers providing bulk paste can retry on a later UI tick.
    pub fn send_input(&self, bytes: &[u8]) -> bool {
        if !self.is_running() {
            return false;
        }
        self.with_inbox(|inbox| {
            if bytes.len() > INPUT_LIMIT.saturating_sub(inbox.input.len()) {
                return false;
            }
            inbox.input.extend(bytes);
            inbox.scroll = isize::MIN; // typing returns to the live viewport
            true
        })
    }
    pub fn paste(&self, text: &str) -> bool {
        if text.len() > INPUT_LIMIT - 12 {
            return false;
        }
        // Normalize clipboard newlines to the terminal's Enter convention.
        let text = text.replace("\r\n", "\n").replace('\n', "\r");
        if self.shared.bracketed_paste.load(Ordering::Relaxed) {
            self.send_input(format!("\x1b[200~{text}\x1b[201~").as_bytes())
        } else {
            self.send_input(text.as_bytes())
        }
    }
    pub fn scroll(&self, lines: isize) -> bool {
        self.with_inbox(|inbox| {
            inbox.scroll = inbox.scroll.saturating_add(lines);
            true
        })
    }
    pub fn handle_key(&self, event: KeyEvent) -> bool {
        if let KeyEvent::Pressed {
            keycode: KeyCode::PageUp | KeyCode::PageDown,
            modifiers,
        } = event
        {
            if modifiers.shift {
                return self.scroll(
                    if matches!(
                        event,
                        KeyEvent::Pressed {
                            keycode: KeyCode::PageUp,
                            ..
                        }
                    ) {
                        10
                    } else {
                        -10
                    },
                );
            }
        }
        match encode_key(
            event,
            self.shared.application_cursor.load(Ordering::Relaxed),
        ) {
            Some(bytes) => self.send_input(&bytes),
            None => false,
        }
    }
    /// Parent should let terminal keyboard events reach this focused view before
    /// applying editor shortcuts (Ctrl+C, Ctrl+D, Ctrl+Z belong to the shell).
    /// `view()` measures its allocated panel and queues PTY resizes automatically.
    pub fn view(&self) -> impl View + Clone + use<> {
        self.view_with_font(None)
    }
    pub fn view_with_font(&self, font: Option<FontStack>) -> impl View + Clone + use<> {
        let terminal = self.clone();
        let mut grid = TextGrid::new(self.shared.grid.clone())
            .cell_size(CELL_WIDTH, CELL_HEIGHT)
            .font_size(14.0)
            .background_color(screen::background())
            .cursor(if self.focused() {
                Some(self.shared.cursor.get())
            } else {
                None
            });
        if let Some(font) = font {
            grid = grid.font_stack(font);
        }
        let content = grid
            .on_event(move |event| {
                if !terminal.focused() {
                    return false;
                }
                match event {
                    Event::Keyboard(key) => terminal.handle_key(*key),
                    Event::TextInputCommit { text, .. } => terminal.send_input(text.as_bytes()),
                    _ => false,
                }
            })
            .focusable(self.shared.focused.clone())
            .frame(f32::INFINITY, f32::INFINITY);
        screen::MeasuredView {
            terminal: self.clone(),
            content,
        }
    }
}

fn encode_key(event: KeyEvent, application_cursor: bool) -> Option<Vec<u8>> {
    let (key, m) = match event {
        KeyEvent::Char { c } => return Some(c.to_string().into_bytes()),
        KeyEvent::Released { .. } => return None,
        KeyEvent::Pressed { keycode, modifiers } => (keycode, modifiers),
    };
    if m.super_key {
        return None;
    }
    let modifier = 1 + u8::from(m.shift) + 2 * u8::from(m.alt) + 4 * u8::from(m.control);
    let final_key = match key {
        KeyCode::Up => Some('A'),
        KeyCode::Down => Some('B'),
        KeyCode::Right => Some('C'),
        KeyCode::Left => Some('D'),
        KeyCode::Home => Some('H'),
        KeyCode::End => Some('F'),
        _ => None,
    };
    if let Some(key) = final_key {
        return Some(
            if modifier != 1 {
                format!("\x1b[1;{modifier}{key}")
            } else {
                format!("\x1b{}{key}", if application_cursor { 'O' } else { '[' })
            }
            .into_bytes(),
        );
    }
    let tilde = match key {
        KeyCode::Insert => Some(2),
        KeyCode::Delete => Some(3),
        KeyCode::PageUp => Some(5),
        KeyCode::PageDown => Some(6),
        KeyCode::F(5) => Some(15),
        KeyCode::F(6) => Some(17),
        KeyCode::F(7) => Some(18),
        KeyCode::F(8) => Some(19),
        KeyCode::F(9) => Some(20),
        KeyCode::F(10) => Some(21),
        KeyCode::F(11) => Some(23),
        KeyCode::F(12) => Some(24),
        _ => None,
    };
    if let Some(code) = tilde {
        return Some(
            if modifier == 1 {
                format!("\x1b[{code}~")
            } else {
                format!("\x1b[{code};{modifier}~")
            }
            .into_bytes(),
        );
    }
    let bytes = match key {
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab if m.shift => b"\x1b[Z".to_vec(),
        KeyCode::Tab => vec![b'\t'],
        KeyCode::Backspace => vec![0x7f],
        KeyCode::Escape => vec![0x1b],
        KeyCode::Space if m.control => vec![0],
        KeyCode::Char(c) if m.control => {
            let c = c.to_ascii_uppercase();
            vec![match c {
                '@'..='_' => c as u8 & 0x1f,
                ' ' | '2' => 0,
                '?' => 0x7f,
                _ => return None,
            }]
        }
        KeyCode::Char(c) if c.is_control() || m.alt => c.to_string().into_bytes(),
        KeyCode::F(n @ 1..=4) => {
            let c = (b'P' + n - 1) as char;
            return Some(
                if modifier == 1 {
                    format!("\x1bO{c}")
                } else {
                    format!("\x1b[1;{modifier}{c}")
                }
                .into_bytes(),
            );
        }
        // Ordinary printable text arrives through Char/IME, not Pressed.
        _ => return None,
    };
    Some(if m.alt {
        [vec![0x1b], bytes].concat()
    } else {
        bytes
    })
}

fn worker(
    cwd: PathBuf,
    size: (u16, u16),
    cancel: Arc<AtomicBool>,
    inbox: Arc<Mutex<Inbox>>,
    output: Arc<Mutex<Option<Snapshot>>>,
) {
    let mut parser = vt100::Parser::new(size.1, size.0, SCROLLBACK_LIMIT);
    let publish = |parser: &vt100::Parser, status| {
        let snapshot = screen::snapshot(parser.screen(), status);
        // One replaceable slot: a slow/hidden UI never holds up PTY draining.
        *output.lock().unwrap() = Some(snapshot);
    };
    if cancel.load(Ordering::Acquire) {
        return;
    }
    let mut pty = match backend::Pty::open(&cwd, size) {
        Ok(pty) => pty,
        Err(error) => {
            publish(&parser, TerminalStatus::Error(error.to_string()));
            return;
        }
    };
    publish(&parser, TerminalStatus::Running);
    let mut pending = VecDeque::new();
    let mut dirty = false;
    let mut last_publish = Instant::now();
    let mut buffer = [0u8; 8192];
    let mut queries = screen::Queries::default();
    let mut eof_since: Option<Instant> = None;
    loop {
        if cancel.load(Ordering::Acquire) {
            break;
        }
        let messages = {
            let mut inbox = inbox.lock().unwrap();
            // The worker-owned pending write queue and UI queue together remain
            // bounded even when the foreground process stops reading input.
            let capacity = INPUT_LIMIT.saturating_sub(pending.len());
            let count = capacity.min(inbox.input.len());
            pending.extend(inbox.input.drain(..count));
            (inbox.size.take(), std::mem::take(&mut inbox.scroll))
        };
        if let Some((cols, rows)) = messages.0 {
            if let Err(error) = pty.resize(cols, rows) {
                publish(&parser, TerminalStatus::Error(format!("resize: {error}")));
                break;
            }
            parser.screen_mut().set_size(rows, cols);
            dirty = true;
        }
        if messages.1 != 0 {
            let offset = parser
                .screen()
                .scrollback()
                .saturating_add_signed(messages.1);
            parser
                .screen_mut()
                .set_scrollback(offset.min(SCROLLBACK_LIMIT));
            dirty = true;
        }
        if !pending.is_empty() {
            let bytes = pending.make_contiguous();
            match pty.write(bytes) {
                Ok(n) => {
                    pending.drain(..n);
                }
                Err(e)
                    if e.kind() == std::io::ErrorKind::WouldBlock
                        || e.kind() == std::io::ErrorKind::Interrupted => {}
                Err(e) => {
                    publish(&parser, TerminalStatus::Error(format!("write: {e}")));
                    break;
                }
            }
        }
        // Bounded work per cycle, so a noisy child cannot starve cancellation,
        // keyboard input, window-size updates or snapshot publication.
        let mut eof = false;
        for _ in 0..16 {
            match pty.read(&mut buffer) {
                Ok(0) => {
                    eof = true;
                    break;
                }
                Ok(n) => {
                    for byte in &buffer[..n] {
                        parser.process(&[*byte]);
                        if let Some(reply) = queries.feed(*byte, parser.screen()) {
                            if pending.len() + reply.len() <= INPUT_LIMIT {
                                pending.extend(reply);
                            }
                        }
                    }
                    dirty = true;
                }
                Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    publish(&parser, TerminalStatus::Error(format!("read: {e}")));
                    return;
                }
            }
        }
        match pty.try_wait() {
            Ok(Some(code)) => {
                publish(&parser, TerminalStatus::Exited(code));
                break;
            }
            Err(e) => {
                publish(&parser, TerminalStatus::Error(format!("wait: {e}")));
                break;
            }
            _ if eof => {
                if eof_since.get_or_insert_with(Instant::now).elapsed()
                    >= Duration::from_millis(500)
                {
                    publish(&parser, TerminalStatus::Exited(None));
                    break;
                }
            }
            _ => {}
        }
        if dirty && last_publish.elapsed() >= Duration::from_millis(16) {
            publish(&parser, TerminalStatus::Running);
            dirty = false;
            last_publish = Instant::now();
        }
        thread::sleep(Duration::from_millis(4));
    }
    // Pty::drop closes the master and terminates/reaps on this worker only.
}
