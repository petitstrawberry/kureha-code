//! The sidebar: a switcher of views (explorer, search, Git, extensions) and
//! the Extensions view, which lists installed extensions with what they
//! contribute and turns each on or off.
use super::*;
use kureha_code::extensions::{self, Extension};
use super::ui::ViewCollection;
use scarlet_ui::prelude::State as Ui;

/// The view the sidebar shows. Search, while open, is shown over it and
/// returns to it when closed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum Sidebar {
    #[default]
    Explorer,
    Git,
    Extensions,
}

/// An installed extension, as the Extensions view lists it.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct ExtensionRow {
    pub id: String,
    pub name: String,
    pub version: String,
    /// What it adds: `2 commands · formatter (python) · theme Kureha Night`.
    pub summary: String,
    pub enabled: bool,
}

/// One line saying what an extension contributes and may do.
pub(super) fn summary(extension: &Extension) -> String {
    let contributes = &extension.manifest.contributes;
    let mut parts = Vec::new();
    let count = |n: usize, one: &str, many: &str| match n {
        0 => None,
        1 => Some(format!("1 {one}")),
        n => Some(format!("{n} {many}")),
    };
    parts.extend(count(contributes.commands.len(), "command", "commands"));
    parts.extend(count(contributes.keybindings.len(), "key", "keys"));
    let formatted: Vec<&str> = contributes
        .formatters
        .iter()
        .flat_map(|f| f.languages.iter().map(String::as_str))
        .collect();
    if !formatted.is_empty() {
        parts.push(format!("formats {}", formatted.join(", ")));
    }
    let languages: Vec<&str> = contributes.languages.iter().map(|l| l.id.as_str()).collect();
    if !languages.is_empty() {
        parts.push(format!("language {}", languages.join(", ")));
    }
    let servers: Vec<&str> = contributes
        .language_servers
        .iter()
        .map(|s| s.language.as_str())
        .collect();
    if !servers.is_empty() {
        parts.push(format!("language server {}", servers.join(", ")));
    }
    let themes: Vec<&str> = extension.themes.iter().map(|t| t.name()).collect();
    if !themes.is_empty() {
        parts.push(format!("theme {}", themes.join(", ")));
    }
    if !extension.manifest.capabilities.process.is_empty() {
        parts.push(format!(
            "may run {}",
            extension.manifest.capabilities.process.join(", ")
        ));
    }
    if parts.is_empty() {
        "Nothing to add yet".into()
    } else {
        parts.join(" · ")
    }
}

/// The Extensions view's own state.
#[derive(Clone)]
pub(super) struct ExtensionsView {
    rows: Ui<Vec<ExtensionRow>>,
    selected: Ui<Option<usize>>,
    /// Problems loading extensions, one per folder.
    errors: Ui<Vec<String>>,
}
impl ExtensionsView {
    pub(super) fn new() -> Self {
        Self {
            rows: Ui::new(generate_state_id(), Vec::new()),
            selected: Ui::new(generate_state_id(), None),
            errors: Ui::new(generate_state_id(), Vec::new()),
        }
    }
    pub(super) fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![&self.rows, &self.errors]
    }
}

impl App {
    /// Shows `view` in the sidebar, closing search.
    pub(super) fn show_sidebar(&self, view: Sidebar) {
        self.project.close();
        if view != Sidebar::Git {
            self.git_panel.blur();
        }
        self.sidebar.set(view);
        if view == Sidebar::Extensions {
            self.refresh_extension_rows();
        }
    }
    pub(super) fn show_explorer(&self) {
        self.show_sidebar(Sidebar::Explorer);
    }
    pub(super) fn show_extensions(&self) {
        self.show_sidebar(Sidebar::Extensions);
    }

    /// Lists the installed extensions again (after loading, or a toggle).
    pub(super) fn refresh_extension_rows(&self) {
        let Some(registry) = super::extension_ui::registry() else {
            return;
        };
        let rows: Vec<ExtensionRow> = registry
            .extensions
            .iter()
            .map(|extension| ExtensionRow {
                id: extension.manifest.id.clone(),
                name: extension.manifest.name.clone(),
                version: extension.manifest.version.clone(),
                summary: summary(extension),
                enabled: extensions::is_enabled(&extension.manifest.id),
            })
            .collect();
        let errors: Vec<String> = registry
            .errors
            .iter()
            .map(|(folder, reason)| {
                let name = folder
                    .file_name()
                    .map_or_else(|| folder.display().to_string(), |n| n.to_string_lossy().into());
                format!("{name}: {reason}")
            })
            .collect();
        if self.extensions_view.rows.get() != rows {
            self.extensions_view.rows.set(rows);
        }
        if self.extensions_view.errors.get() != errors {
            self.extensions_view.errors.set(errors);
        }
    }
    #[cfg(test)]
    pub(super) fn extensions_view_rows(&self) -> Vec<ExtensionRow> {
        self.extensions_view.rows.get()
    }
    /// Turns an extension on or off from the Extensions view.
    pub(super) fn toggle_extension(&self, id: &str) {
        let enabled = extensions::is_enabled(id);
        self.set_extension_enabled(id, !enabled);
        self.refresh_extension_rows();
    }

    /// The switcher above the sidebar.
    pub(super) fn sidebar_switcher(&self) -> impl View + Clone + use<> {
        let colors = self.theme.get().colors();
        let searching = self.project.is_open();
        let current = self.sidebar.get();
        // (icon, shown, action); the pinned icon set has no branch symbol.
        type Show = Box<dyn Fn(&App)>;
        let items: [(Icon, bool, Show); 4] = [
            (Icon::Folder, !searching && current == Sidebar::Explorer, Box::new(App::show_explorer)),
            (Icon::Search, searching, Box::new(App::show_project_search)),
            (Icon::Share, !searching && current == Sidebar::Git, Box::new(App::show_git)),
            (Icon::Package, !searching && current == Sidebar::Extensions, Box::new(App::show_extensions)),
        ];
        let buttons = items.into_iter().map(|(icon, shown, action)| {
            let app = self.clone();
            Button::icon_only(icon)
                .header_style()
                .text_color(if shown { colors.foreground } else { colors.muted })
                .background_color(if shown { colors.selection } else { Color::TRANSPARENT })
                .padding(6.0)
                .on_click(move || action(&app))
        });
        hstack! {
            HStack::new(ViewCollection(buttons.collect())).spacing(4.0),
            Spacer::new(),
        }
        .padding_insets(EdgeInsets::new(12.0, 10.0, 8.0, 2.0))
    }

    /// The view below the switcher.
    pub(super) fn sidebar_view(&self) -> impl View + Clone + use<> {
        if_view!(self.project.is_open(), self.project_search_view(),
            if_view!(self.sidebar.get() == Sidebar::Git, self.git_view(),
                if_view!(self.sidebar.get() == Sidebar::Extensions, self.extensions_panel(),
                    self.explorer_view())))
    }

    fn extensions_panel(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let colors = theme.colors();
        let loaded = super::extension_ui::registry().is_some();
        let rows = self.extensions_view.rows.get();
        let errors = self.extensions_view.errors.get();
        let empty = if !loaded {
            "Loading extensions…".to_string()
        } else if rows.is_empty() {
            match extensions::extensions_dir() {
                Some(dir) => format!("No extensions installed. Put each in its own folder in {}", dir.display()),
                None => "No extensions installed".into(),
            }
        } else {
            String::new()
        };
        let refresh = self.clone();
        let list = self.clone();
        let header = hstack! {
            Text::new("Extensions").font_size(13.0).color(colors.foreground),
            Spacer::new(),
            Button::icon_only(Icon::Refresh).header_style().text_color(colors.foreground)
                .background_color(Color::TRANSPARENT).padding(5.0)
                .on_click(move || refresh.refresh_extension_rows()),
        }
        .spacing(2.0);
        let row_colors = colors;
        vstack! {
            header,
            // Why the list is empty, load problems, and what a toggle does.
            VStack::new(ViewCollection(
                (!empty.is_empty()).then(|| (empty.clone(), colors.muted, None)).into_iter()
                    .chain(errors.into_iter().map(|error| (error, colors.error, Some(3))))
                    .chain(std::iter::once((
                        "Changes apply at once; installing or removing extensions needs a restart.".to_string(),
                        colors.muted,
                        None,
                    )))
                    .map(|(text, color, lines)| Text::new(text).font_size(11.0).color(color).line_limit(lines))
                    .collect(),
            )).alignment(Alignment::Leading).spacing(4.0),
            ListView::new(self.extensions_view.rows.clone(), self.extensions_view.selected.clone(), 64.0,
                move |_, row: ExtensionRow, _| list.extension_row(row, row_colors))
                .item_key(|row| row.id.clone().into())
                .frame(f32::INFINITY, f32::INFINITY),
        }
        .alignment(Alignment::Leading)
        .spacing(8.0)
        .padding_insets(EdgeInsets::new(12.0, 4.0, 12.0, 12.0))
        .frame(f32::INFINITY, f32::INFINITY)
    }

    fn extension_row(
        &self,
        row: ExtensionRow,
        colors: kureha_code::theme::WorkbenchColors,
    ) -> impl View + Clone + use<> {
        let toggle = self.clone();
        let id = row.id.clone();
        let text = if row.enabled { colors.foreground } else { colors.muted };
        hstack! {
            vstack! {
                Text::new(row.name.clone()).font_size(13.0).color(text),
                Text::new(format!("{} · {}", row.id, row.version)).font_size(11.0).color(colors.muted)
                    .truncation(scarlet_ui::views::Truncation::Middle),
                Text::new(row.summary.clone()).font_size(11.0).color(colors.muted),
            }
            .alignment(Alignment::Leading)
            .spacing(2.0),
            Spacer::new(),
            Button::new(if row.enabled { "Disable" } else { "Enable" }).header_style()
                .font_size(11.0).padding(4.0).text_color(colors.foreground)
                .background_color(Color::TRANSPARENT)
                .on_click(move || toggle.toggle_extension(&id)),
        }
        .spacing(6.0)
        .padding_insets(EdgeInsets::new(2.0, 6.0, 0.0, 6.0))
        .frame(f32::INFINITY, 64.0)
    }
}
