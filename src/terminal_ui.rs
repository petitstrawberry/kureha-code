//! Shell sessions survive moves between the bottom panel and editor groups.
use super::*;
use kureha_code::terminal::Terminal;
use scarlet_ui::event::{MouseButton, MouseEvent};

pub(super) fn session_label(id: u64) -> String {
    let shell = std::env::var_os("SHELL")
        .and_then(|shell| {
            Path::new(&shell)
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "sh".into());
    format!("{shell} {id}")
}
#[derive(Clone)]
pub(super) struct Session {
    pub id: u64,
    pub terminal: Terminal,
    directory: PathBuf,
    pub tab: Option<TabId>,
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
    pub fn panel_sessions(&self) -> Vec<Session> {
        self.sessions()
            .into_iter()
            .filter(|session| session.tab.is_none())
            .collect()
    }
    pub fn for_tab(&self, tab: TabId) -> Option<Session> {
        self.sessions()
            .into_iter()
            .find(|session| session.tab == Some(tab))
    }
    pub fn session(&self, id: u64) -> Option<Session> {
        self.sessions().into_iter().find(|session| session.id == id)
    }
    pub fn focused_session(&self) -> Option<Session> {
        self.sessions()
            .into_iter()
            .find(|session| session.terminal.focused())
    }
    pub fn set_tab(&self, id: u64, tab: Option<TabId>) {
        let mut model = self.model.lock().unwrap();
        let Some(session) = model.sessions.iter_mut().find(|s| s.id == id) else {
            return;
        };
        session.tab = tab;
        model.epoch += 1;
        if model.active == Some(id) && tab.is_some() {
            model.active = model
                .sessions
                .iter()
                .find(|s| s.tab.is_none())
                .map(|s| s.id);
        }
        drop(model);
        self.changed();
    }
    pub fn active(&self) -> Option<Session> {
        let model = self.model.lock().unwrap();
        model
            .sessions
            .iter()
            .find(|s| Some(s.id) == model.active && s.tab.is_none())
            .cloned()
    }
    pub fn add(&self, directory: PathBuf) -> u64 {
        self.start(directory, None)
    }
    fn start(&self, directory: PathBuf, tab: Option<TabId>) -> u64 {
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
            tab,
        });
        if tab.is_none() {
            model.active = Some(id);
        }
        drop(model);
        terminal.focus();
        self.changed();
        id
    }
    pub fn select(&self, id: u64) {
        self.blur();
        let mut model = self.model.lock().unwrap();
        let Some(session) = model.sessions.iter().find(|s| s.id == id).cloned() else {
            return;
        };
        if session.tab.is_none() {
            model.active = Some(id);
        }
        drop(model);
        session.terminal.focus();
        self.changed();
    }
    pub fn next(&self, backwards: bool) {
        let sessions = self.panel_sessions();
        if sessions.is_empty() {
            return;
        }
        let active = self.active().map(|s| s.id);
        let index = sessions
            .iter()
            .position(|s| Some(s.id) == active)
            .unwrap_or(0);
        self.select(
            sessions[(index + if backwards { sessions.len() - 1 } else { 1 }) % sessions.len()].id,
        );
    }
    pub fn close(&self, id: u64) -> bool {
        let mut model = self.model.lock().unwrap();
        let Some(index) = model.sessions.iter().position(|s| s.id == id) else {
            return model.sessions.iter().any(|s| s.tab.is_none());
        };
        model.sessions.remove(index).terminal.close();
        model.epoch += 1;
        if model.active == Some(id) {
            model.active = model
                .sessions
                .iter()
                .filter(|s| s.tab.is_none())
                .map(|s| s.id)
                .next();
        }
        let remaining = model.sessions.iter().any(|s| s.tab.is_none());
        drop(model);
        self.focus();
        self.changed();
        remaining
    }
    pub fn focus(&self) {
        if let Some(session) = self.active() {
            self.blur();
            session.terminal.focus();
        }
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
    pub fn restart(&self, id: u64) {
        if let Some(session) = self.session(id) {
            session.terminal.open(session.directory);
            self.select(id);
            self.changed();
        }
    }
    pub fn paste_target(&self) -> Option<PasteTarget> {
        let model = self.model.lock().unwrap();
        let session = model.sessions.iter().find(|s| s.terminal.focused())?;
        Some(PasteTarget {
            id: session.id,
            generation: session.terminal.generation(),
            epoch: model.epoch,
        })
    }
    pub fn paste(&self, target: PasteTarget, text: &str) -> bool {
        self.paste_target() == Some(target)
            && self
                .session(target.id)
                .is_some_and(|s| s.terminal.paste(text))
    }
}
impl App {
    pub(super) fn blur_terminals(&self) {
        self.terminal.blur();
    }
    pub(super) fn terminal_focused(&self) -> bool {
        self.terminal
            .focused_session()
            .is_some_and(|session| match session.tab {
                Some(tab) => self
                    .pane_order()
                    .into_iter()
                    .any(|g| self.group_id(g) == Some(tab)),
                None => self.panel_visible.get(),
            })
    }
    pub(super) fn poll_terminals(&self) {
        self.terminal.poll();
    }
    pub(super) fn terminal_paste_target(&self) -> Option<PasteTarget> {
        self.terminal_focused()
            .then(|| self.terminal.paste_target())
            .flatten()
    }
    pub(super) fn apply_terminal_paste(&self, target: PasteTarget, text: &str) -> bool {
        self.terminal_focused() && self.terminal.paste(target, text)
    }
    pub(super) fn restart_terminal(&self) {
        let session = self
            .terminal
            .focused_session()
            .or_else(|| {
                self.group_id(self.focused_group.get())
                    .and_then(|tab| self.terminal.for_tab(tab))
            })
            .or_else(|| self.terminal.active());
        if let Some(session) = session {
            self.terminal.restart(session.id);
        }
    }
    pub(super) fn focus_group_terminal(&self, group: usize) {
        self.terminal.blur();
        if let Some(session) = self
            .group_id(group)
            .and_then(|tab| self.terminal.for_tab(tab))
        {
            self.terminal.select(session.id);
        }
    }
    pub(super) fn attach_terminal(&self, session: u64, group: usize, index: usize) {
        if self.blocked() || !self.group_exists(group) {
            return;
        }
        let Some(session) = self.terminal.session(session).filter(|s| s.tab.is_none()) else {
            return;
        };
        let tab = {
            let mut rt = self.runtime.lock().unwrap();
            let tab = rt.workspace.new_tab_id();
            let tabs = &mut rt.group_tabs[group];
            tabs.insert(index.min(tabs.len()), tab);
            tab
        };
        self.terminal.set_tab(session.id, Some(tab));
        self.focus_group(group);
        self.activate_unchecked(tab);
        self.changed();
    }
    pub(super) fn split_terminal_from_panel(
        &self,
        session: u64,
        beside: Option<usize>,
        edge: tab_drag::Edge,
    ) {
        if self.blocked() || beside.is_some_and(|g| !self.group_exists(g)) {
            return;
        }
        let Some(session) = self.terminal.session(session).filter(|s| s.tab.is_none()) else {
            return;
        };
        let tab = self.runtime.lock().unwrap().workspace.new_tab_id();
        self.terminal.set_tab(session.id, Some(tab));
        let group = self.terminal_group(tab, beside, edge);
        self.focus_group(group);
        self.changed();
    }
    fn terminal_group(&self, tab: TabId, beside: Option<usize>, edge: tab_drag::Edge) -> usize {
        if let Some(group) = beside {
            self.add_group(tab, group, edge)
        } else {
            self.remember_pane_sizes();
            let group = self.allocate_group(tab);
            self.pane_layout.update(|layout| layout.wrap(group, edge));
            group
        }
    }
    pub(super) fn split_terminal(&self, tab: TabId, down: bool) {
        let Some(session) = self.terminal.for_tab(tab) else {
            return;
        };
        let next = self.runtime.lock().unwrap().workspace.new_tab_id();
        self.terminal.start(session.directory, Some(next));
        let group = self.add_group(
            next,
            self.focused_group.get(),
            if down {
                tab_drag::Edge::Bottom
            } else {
                tab_drag::Edge::Right
            },
        );
        self.focus_group(group);
        self.changed();
    }
    fn remove_terminal_tab_views(&self, tab: TabId) {
        let mut panes = self.panes.get();
        {
            let mut rt = self.runtime.lock().unwrap();
            for (group, tabs) in rt.group_tabs.iter_mut().enumerate() {
                let index = tabs.iter().position(|id| *id == tab).unwrap_or(0);
                tabs.retain(|id| *id != tab);
                if panes[group] == Some(tab) {
                    panes[group] = tabs.get(index.min(tabs.len().saturating_sub(1))).copied();
                }
            }
        }
        self.panes.set(panes);
        for group in self.pane_order() {
            self.retire_empty_group(group);
        }
        self.invalidate_clipboard();
    }
    pub(super) fn return_terminal_to_panel(&self, tab: TabId) {
        if self.blocked() {
            return;
        }
        let Some(session) = self.terminal.for_tab(tab) else {
            return;
        };
        self.remove_terminal_tab_views(tab);
        self.terminal.set_tab(session.id, None);
        self.panel_visible.set(true);
        self.terminal.select(session.id);
        self.changed();
    }
    pub(super) fn close_terminal_tab(&self, tab: TabId) -> bool {
        let Some(session) = self.terminal.for_tab(tab) else {
            return false;
        };
        if self.blocked() {
            return true;
        }
        self.remove_terminal_tab_views(tab);
        self.terminal.close(session.id);
        self.focus_group_terminal(self.focused_group.get());
        self.changed();
        true
    }
    fn focus_terminal_session(&self, id: u64) -> bool {
        if self.blocked() {
            return false;
        }
        let Some(session) = self.terminal.session(id) else {
            return false;
        };
        if let Some(tab) = session.tab {
            let Some(group) = self
                .pane_order()
                .into_iter()
                .find(|g| self.group_id(*g) == Some(tab))
            else {
                return false;
            };
            self.focus_group(group);
        } else if !self.panel_visible.get() {
            return false;
        }
        self.project.blur();
        self.terminal.select(id);
        true
    }
    fn terminal_context_menu(&self, id: u64) -> scarlet_ui::views::Menu {
        use scarlet_ui::views::{Menu, MenuItemContent};
        let copy = self.clone();
        let paste = self.clone();
        Menu::new(vec![
            MenuItemContent::new("Copy Visible Output").callback(move || {
                if !copy.focus_terminal_session(id) {
                    return;
                }
                let Some(session) = copy.terminal.session(id) else {
                    return;
                };
                let text = session.terminal.visible_text();
                if text.is_empty() {
                    return;
                }
                let Some(service) = copy.clipboard_service() else {
                    return;
                };
                let handle = service.write_text(text);
                copy.runtime.lock().unwrap().clipboard_job =
                    Some(ClipboardJob::TerminalCopy { handle });
            }),
            MenuItemContent::new("Paste")
                .shortcut(if cfg!(target_os = "macos") {
                    "Cmd+V"
                } else {
                    "Ctrl+Shift+V"
                })
                .callback(move || {
                    if paste.focus_terminal_session(id) {
                        paste.paste_terminal();
                    }
                }),
        ])
    }
    pub(super) fn terminal_tab_view(&self, tab: TabId, group: usize) -> impl View + Clone + use<> {
        let session = self.terminal.for_tab(tab).unwrap();
        let palette = self.theme.get().terminal_palette();
        session.terminal.set_palette(palette);
        let focus = self.clone();
        Surface::section(
            session
                .terminal
                .view_with_font(self.document_font())
                .context_menu(self.terminal_context_menu(session.id))
                .padding(10.0)
                .background(palette.background),
        )
        .fill(palette.background)
        .corner_radius(12.0)
        .clip_content(true)
        .frame(f32::INFINITY, f32::INFINITY)
        .padding(4.0)
        .on_event_capture(move |event| {
            if matches!(
                event,
                Event::Mouse(MouseEvent::ButtonPressed {
                    button: MouseButton::Left,
                    ..
                })
            ) {
                focus.focus_group(group);
            }
            false
        })
    }
    fn terminal_directory(&self) -> PathBuf {
        let id = self.active_id();
        let rt = self.runtime.lock().unwrap();
        rt.explorer
            .root
            .clone()
            .or_else(|| {
                id.and_then(|id| rt.workspace.get(id))
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
        if self
            .terminal
            .focused_session()
            .is_some_and(|s| s.tab.is_none())
        {
            self.focus_group_terminal(self.focused_group.get());
        }
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
        let sessions = self.terminal.panel_sessions().into_iter().map(|session| {
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
        let palette = theme.terminal_palette();
        let background = palette.background;
        let body = if let Some(session) = active {
            session.terminal.set_palette(palette);
            Either::A(
                session
                    .terminal
                    .view_with_font(self.document_font())
                    .context_menu(self.terminal_context_menu(session.id))
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
        let app = App::with_document();
        let document = app.document_id();
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
        assert_eq!(app.document_id(), document);
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
    #[cfg(all(unix, not(target_os = "scarlet")))]
    fn editor_terminal_moves_preserve_shell_state_and_panel_focus_is_independent() {
        let app = App::with_document();
        let document = app.document_id();
        app.split_editor(true);
        app.new_terminal();
        let shell = app.terminal.active().unwrap();
        wait(&app.terminal, &shell.terminal, |t| {
            t.status() == TerminalStatus::Running
        });
        shell
            .terminal
            .send_input(b"stty -echo; KUREHA_MOVE=preserved; printf 'READY_%s\\n' move\r");
        wait(&app.terminal, &shell.terminal, |t| {
            output(t).contains("READY_move")
        });
        let generation = shell.terminal.generation();
        app.new_terminal();
        let panel = app.terminal.active().unwrap();
        app.attach_terminal(shell.id, 1, 0);
        let tab = app.group_id(1).unwrap();
        let layout = app.pane_layout.get();
        assert_eq!(app.terminal.panel_sessions().len(), 1);
        assert_eq!(app.terminal.active().unwrap().id, panel.id);
        assert!(shell.terminal.focused());
        assert!(!panel.terminal.focused());
        app.hide_panel();
        assert!(shell.terminal.focused());
        app.toggle_panel();
        assert!(panel.terminal.focused());
        assert!(!shell.terminal.focused());
        app.hide_panel();
        assert!(shell.terminal.focused());
        assert_eq!(app.pane_layout.get(), layout);
        app.show_commands();
        assert!(!shell.terminal.focused());
        app.close_navigation();
        assert!(shell.terminal.focused());
        assert!(!panel.terminal.focused());
        app.activate(document);
        assert!(!shell.terminal.focused());
        app.activate(tab);
        assert!(shell.terminal.focused());
        let stale = app.terminal_paste_target().unwrap();
        app.move_tab(tab, 1, 0, 0);
        assert_eq!(app.group_id(0), Some(tab));
        assert!(!app.apply_terminal_paste(stale, "stale after move"));
        assert_eq!(shell.terminal.generation(), generation);
        shell
            .terminal
            .send_input(b"printf 'MOVED_%s\\n' \"$KUREHA_MOVE\"\r");
        wait(&app.terminal, &shell.terminal, |t| {
            output(t).contains("MOVED_preserved")
        });
        app.return_terminal_to_panel(tab);
        assert_eq!(app.terminal.active().unwrap().id, shell.id);
        assert_eq!(app.terminal.panel_sessions().len(), 2);
        assert!(app.panel_visible.get());
        app.attach_terminal(shell.id, 0, 0);
        let tab = app.group_id(0).unwrap();
        app.request_close(Some(tab));
        assert_eq!(shell.terminal.status(), TerminalStatus::Closed);
        assert!(app.terminal.session(shell.id).is_none());
        wait(&app.terminal, &panel.terminal, |t| {
            t.status() == TerminalStatus::Running
        });
        assert_eq!(app.terminal.panel_sessions().len(), 1);
        panel.terminal.close();
    }

    #[test]
    fn editor_terminal_split_starts_an_independent_shell_and_closing_group_keeps_it() {
        let app = App::with_document();
        app.new_terminal();
        let original = app.terminal.active().unwrap();
        app.attach_terminal(original.id, 0, 0);
        let first_tab = app.group_id(0).unwrap();
        app.split_editor(false);
        let second_tab = app.group_id(app.focused_group.get()).unwrap();
        let second = app.terminal.for_tab(second_tab).unwrap();
        assert_ne!(original.id, second.id);
        assert_ne!(first_tab, second_tab);
        assert_eq!(app.terminal.sessions().len(), 2);
        assert!(app.terminal.panel_sessions().is_empty());
        app.close_split();
        assert_eq!(app.pane_order(), vec![0]);
        assert!(app.runtime.lock().unwrap().group_tabs[0].contains(&second_tab));
        assert_eq!(app.terminal.sessions().len(), 2);
        app.activate(second_tab);
        assert!(second.terminal.focused());
        assert!(!original.terminal.focused());
        app.request_close(Some(second_tab));
        assert_eq!(second.terminal.status(), TerminalStatus::Closed);
        assert!(app.terminal.for_tab(first_tab).is_some());
        original.terminal.close();
    }

    #[test]
    fn panel_toggle_is_visibility_based_and_file_operations_never_enter_panel() {
        let app = App::with_document();
        app.new_terminal();
        let id = app.document_id();
        let layout = app.pane_layout.get();
        app.focus_group(0); // Panel is visible, but editor has focus.
        app.toggle_panel();
        assert!(!app.panel_visible.get());
        app.toggle_panel();
        app.new_document();
        assert_ne!(app.document_id(), id);
        assert!(!app.terminal_focused());
        assert_eq!(app.pane_layout.get(), layout);
        assert_eq!(app.runtime.lock().unwrap().workspace.documents.len(), 2);
        assert_eq!(app.terminal.sessions().len(), 1);
        app.hide_panel();
        app.toggle_panel();
        assert!(app.panel_visible.get());
        assert_eq!(app.pane_layout.get(), layout);
        for session in app.terminal.sessions() {
            session.terminal.close();
        }
    }
}
