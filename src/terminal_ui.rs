//! The utility panel owns shell sessions independently of editor documents,
//! navigation history and the editor split tree.
use super::*;
use kureha_code::terminal::Terminal;
use scarlet_ui::event::{MouseButton, MouseEvent};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PanelPosition {
    Bottom,
    Right,
}
#[derive(Clone)]
pub(super) struct Session {
    pub id: u64,
    pub terminal: Terminal,
    directory: PathBuf,
}
#[derive(Default)]
struct Model {
    sessions: Vec<Session>,
    active: Option<u64>,
    next_id: u64,
    epoch: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct PasteTarget {
    id: u64,
    generation: u64,
    epoch: u64,
}
#[derive(Clone)]
pub(super) struct Panel {
    model: Arc<Mutex<Model>>,
    pub revision: State<u64>,
}
impl Panel {
    pub fn new() -> Self {
        Self {
            model: Arc::new(Mutex::new(Model::default())),
            revision: State::new(generate_state_id(), 0),
        }
    }
    fn changed(&self) {
        self.revision.update(|r| *r = r.wrapping_add(1));
    }
    pub fn sessions(&self) -> Vec<Session> {
        self.model.lock().unwrap().sessions.clone()
    }
    pub fn active(&self) -> Option<Session> {
        let model = self.model.lock().unwrap();
        model
            .sessions
            .iter()
            .find(|s| Some(s.id) == model.active)
            .cloned()
    }
    pub fn add(&self, directory: PathBuf) {
        self.blur();
        let terminal = Terminal::new();
        terminal.open(&directory);
        let mut model = self.model.lock().unwrap();
        model.next_id += 1;
        let id = model.next_id;
        model.sessions.push(Session {
            id,
            terminal: terminal.clone(),
            directory,
        });
        model.active = Some(id);
        drop(model);
        terminal.focus();
        self.changed();
    }
    pub fn select(&self, id: u64) {
        self.blur();
        let mut model = self.model.lock().unwrap();
        if model.sessions.iter().any(|s| s.id == id) {
            model.active = Some(id);
        }
        drop(model);
        self.focus();
        self.changed();
    }
    pub fn next(&self, backwards: bool) {
        let model = self.model.lock().unwrap();
        let len = model.sessions.len();
        if len == 0 {
            return;
        }
        let index = model
            .sessions
            .iter()
            .position(|s| Some(s.id) == model.active)
            .unwrap_or(0);
        let id = model.sessions[(index + if backwards { len - 1 } else { 1 }) % len].id;
        drop(model);
        self.select(id);
    }
    pub fn close(&self, id: u64) -> bool {
        let mut model = self.model.lock().unwrap();
        let Some(index) = model.sessions.iter().position(|s| s.id == id) else {
            return !model.sessions.is_empty();
        };
        model.sessions.remove(index).terminal.close();
        model.epoch += 1;
        if model.active == Some(id) {
            model.active = model
                .sessions
                .get(index.min(model.sessions.len().saturating_sub(1)))
                .map(|s| s.id);
        }
        let remaining = !model.sessions.is_empty();
        drop(model);
        self.focus();
        self.changed();
        remaining
    }
    pub fn focus(&self) {
        if let Some(session) = self.active() {
            session.terminal.focus();
        }
    }
    pub fn focused(&self) -> bool {
        self.active().is_some_and(|s| s.terminal.focused())
    }
    pub fn blur(&self) {
        let mut model = self.model.lock().unwrap();
        model.epoch += 1;
        for session in &model.sessions {
            session.terminal.blur();
        }
    }
    pub fn poll(&self) {
        for session in self.sessions() {
            session.terminal.poll();
        }
    }
    pub fn restart(&self) {
        if let Some(session) = self.active() {
            session.terminal.open(session.directory);
            session.terminal.focus();
            self.changed();
        }
    }
    pub fn paste_target(&self) -> Option<PasteTarget> {
        let model = self.model.lock().unwrap();
        let session = model
            .sessions
            .iter()
            .find(|s| Some(s.id) == model.active && s.terminal.focused())?;
        Some(PasteTarget {
            id: session.id,
            generation: session.terminal.generation(),
            epoch: model.epoch,
        })
    }
    pub fn paste(&self, target: PasteTarget, text: &str) -> bool {
        self.paste_target() == Some(target) && self.active().is_some_and(|s| s.terminal.paste(text))
    }
}
impl App {
    pub(super) fn blur_terminals(&self) {
        self.terminal.blur();
    }
    pub(super) fn terminal_focused(&self) -> bool {
        self.panel_visible.get() && self.terminal.focused()
    }
    pub(super) fn poll_terminals(&self) {
        self.terminal.poll();
    }
    pub(super) fn terminal_paste_target(&self) -> Option<PasteTarget> {
        self.panel_visible
            .get()
            .then(|| self.terminal.paste_target())
            .flatten()
    }
    pub(super) fn apply_terminal_paste(&self, target: PasteTarget, text: &str) -> bool {
        self.panel_visible.get() && self.terminal.paste(target, text)
    }
    fn terminal_directory(&self) -> PathBuf {
        let id = self.active_id();
        let rt = self.runtime.lock().unwrap();
        rt.explorer
            .root
            .clone()
            .or_else(|| {
                rt.workspace
                    .get(id)
                    .and_then(|doc| doc.path.as_ref()?.parent().map(Path::to_path_buf))
            })
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/"))
    }
    pub(super) fn toggle_panel(&self) {
        if self.blocked() {
            return;
        }
        if self.panel_visible.get() {
            self.hide_panel();
        } else {
            self.close_navigation();
            self.dismiss_language_panel();
            self.project.blur();
            if self.terminal.active().is_none() {
                self.terminal.add(self.terminal_directory());
            }
            self.panel_visible.set(true);
            self.terminal.focus();
        }
    }
    pub(super) fn hide_panel(&self) {
        self.remember_panel_size();
        self.panel_visible.set(false);
        self.terminal.blur();
    }
    pub(super) fn new_terminal(&self) {
        if self.blocked() {
            return;
        }
        self.close_navigation();
        self.dismiss_language_panel();
        self.project.blur();
        self.terminal.add(self.terminal_directory());
        self.panel_visible.set(true);
    }
    pub(super) fn terminal_panel(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let colors = theme.colors();
        let active = self.terminal.active();
        let active_id = active.as_ref().map(|s| s.id);
        let shell = std::env::var_os("SHELL")
            .and_then(|shell| {
                Path::new(&shell)
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
            })
            .unwrap_or_else(|| "sh".into());
        let mut width = 0.0;
        let mut selected = (0.0, 0.0);
        let sessions = self.terminal.sessions().into_iter().map(|session| {
            let id = session.id; let select = self.clone(); let close = self.clone();
            if active_id == Some(id) { selected = (width, width + 132.0); } width += 136.0;
            let surface = Surface::section(hstack! {
                Button::new(format!("{shell} {id}")).header_style().icon(Icon::Terminal).font_size(12.0).padding(5.0)
                    .text_color(colors.foreground).background_color(Color::TRANSPARENT)
                    .on_click(move || select.terminal.select(id)).frame(104.0, 30.0),
                Button::new("×").header_style().padding(4.0).text_color(colors.muted).background_color(Color::TRANSPARENT)
                    .on_click(move || { if !close.terminal.close(id) { close.hide_panel(); } }).frame(28.0, 30.0),
            }.spacing(0.0)).corner_radius(9.0).clip_content(true);
            self.tab_region(if Some(id) == active_id { surface.surface_style(theme.tab_style()) }
            else { surface.fill(Color::TRANSPARENT) }, tab_drag::RegionId::Session(id)).key(id)
        }).collect::<Vec<_>>();
        let new = self.clone();
        let hide = self.clone();
        let focus = self.clone();
        let background = kureha_code::terminal::background_color();
        let body = if let Some(session) = active {
            Either::A(
                session
                    .terminal
                    .view_with_font(self.document_font())
                    .key(session.id),
            )
        } else {
            Either::B(VStack::new(()))
        };
        self.tab_region(Surface::section(vstack! {
            hstack! {
                ScrollView::new(HStack::new(super::ui::ViewCollection(sessions)).spacing(4.0)).horizontal()
                    .content_size(width, 30.0).scroll_to_horizontal_range(selected.0, selected.1).frame(f32::INFINITY, 32.0),
                Button::icon_only(Icon::Plus).header_style().text_color(colors.foreground).background_color(Color::TRANSPARENT)
                    .on_click(move || new.new_terminal()).frame(28.0, 30.0),
                Button::new("×").header_style().text_color(colors.foreground).background_color(Color::TRANSPARENT)
                    .on_click(move || hide.hide_panel()).frame(28.0, 30.0),
            }.spacing(4.0).padding_insets(EdgeInsets::new(10.0, 8.0, 8.0, 6.0)),
            Surface::section(body.padding(10.0).background(background)).fill(background).corner_radius(12.0).clip_content(true)
                .frame(f32::INFINITY, f32::INFINITY).padding_insets(EdgeInsets::new(6.0, 0.0, 6.0, 6.0))
                .on_event_capture(move |event| { if matches!(event, Event::Mouse(MouseEvent::ButtonPressed { button: MouseButton::Left, .. })) { focus.terminal.focus(); focus.project.blur(); } false }),
        }.spacing(0.0).frame(f32::INFINITY, f32::INFINITY)).surface_style(theme.chrome_style()).corner_radius(16.0).clip_content(true), tab_drag::RegionId::Panel)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kureha_code::terminal::TerminalStatus;
    use std::time::{Duration, Instant};
    fn output(terminal: &Terminal) -> String {
        let grid = terminal.grid();
        (0..grid.rows())
            .map(|r| {
                (0..grid.columns())
                    .filter_map(|c| grid.cell(c, r))
                    .map(|c| c.ch)
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }
    fn wait(panel: &Panel, terminal: &Terminal, predicate: impl Fn(&Terminal) -> bool) {
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            panel.poll();
            if predicate(terminal) {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "PTY timed out: {:?}\n{}",
                terminal.status(),
                output(terminal)
            );
            thread::sleep(Duration::from_millis(10));
        }
    }
    #[test]
    #[cfg(all(unix, not(target_os = "scarlet")))]
    fn independent_sessions_keep_shell_state_when_hidden_and_close_only_the_selected_pty() {
        let app = App::new();
        let document = app.active_id();
        app.split_editor(true);
        app.focus_group(0);
        app.split_editor(false);
        let layout = app.pane_layout.get();
        let tabs = app.runtime.lock().unwrap().group_tabs.clone();
        app.toggle_panel();
        let first = app.terminal.active().unwrap();
        wait(&app.terminal, &first.terminal, |t| {
            t.status() == TerminalStatus::Running
        });
        first.terminal.send_input(b"exec /bin/sh -i\r");
        first
            .terminal
            .send_input(b"stty -echo; KUREHA_SESSION=alpha; printf 'READY_%s\\n' alpha\r");
        wait(&app.terminal, &first.terminal, |t| {
            output(t).contains("READY_alpha")
        });
        let generation = first.terminal.generation();
        app.new_terminal();
        let second = app.terminal.active().unwrap();
        wait(&app.terminal, &second.terminal, |t| {
            t.status() == TerminalStatus::Running
        });
        second.terminal.send_input(b"exec /bin/sh -i\r");
        second
            .terminal
            .send_input(b"stty -echo; KUREHA_SESSION=beta; printf 'READY_%s\\n' beta\r");
        wait(&app.terminal, &second.terminal, |t| {
            output(t).contains("READY_beta")
        });
        app.hide_panel();
        first
            .terminal
            .send_input(b"printf 'HIDDEN_%s\\n' \"$KUREHA_SESSION\"\r");
        wait(&app.terminal, &first.terminal, |t| {
            output(t).contains("HIDDEN_alpha")
        });
        assert_eq!(app.pane_layout.get(), layout);
        assert_eq!(app.runtime.lock().unwrap().group_tabs, tabs);
        assert_eq!(app.active_id(), document);
        assert_eq!(app.runtime.lock().unwrap().workspace.documents.len(), 1);
        app.toggle_panel();
        app.terminal.select(first.id);
        assert_eq!(first.terminal.generation(), generation);
        let paste = app.terminal_paste_target().unwrap();
        app.terminal.select(second.id);
        assert!(!app.apply_terminal_paste(paste, "wrong session"));
        app.terminal.select(first.id);
        assert!(!app.apply_terminal_paste(paste, "returned after navigation"));
        let paste = app.terminal_paste_target().unwrap();
        app.hide_panel();
        app.toggle_panel();
        assert!(!app.apply_terminal_paste(paste, "hidden meanwhile"));
        assert!(app.terminal.close(first.id));
        assert_eq!(first.terminal.status(), TerminalStatus::Closed);
        second
            .terminal
            .send_input(b"printf 'ALIVE_%s\\n' \"$KUREHA_SESSION\"\r");
        wait(&app.terminal, &second.terminal, |t| {
            output(t).contains("ALIVE_beta")
        });
        assert!(!app.terminal.close(second.id));
        assert_eq!(second.terminal.status(), TerminalStatus::Closed);
    }
    #[test]
    fn panel_toggle_is_visibility_based_and_file_operations_never_enter_panel() {
        let app = App::new();
        app.new_terminal();
        let id = app.active_id();
        let layout = app.pane_layout.get();
        app.focus_group(0); // Panel is visible, but editor has focus.
        app.toggle_panel();
        assert!(!app.panel_visible.get());
        app.toggle_panel();
        app.new_document();
        assert_ne!(app.active_id(), id);
        assert!(!app.terminal_focused());
        assert_eq!(app.pane_layout.get(), layout);
        assert_eq!(app.runtime.lock().unwrap().workspace.documents.len(), 2);
        assert_eq!(app.terminal.sessions().len(), 1);
        app.panel_position.set(PanelPosition::Right);
        app.hide_panel();
        app.toggle_panel();
        assert_eq!(app.panel_position.get(), PanelPosition::Right);
        assert_eq!(app.pane_layout.get(), layout);
        for session in app.terminal.sessions() {
            session.terminal.close();
        }
    }
}
