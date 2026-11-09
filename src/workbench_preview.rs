//! Reproducible visual fixtures, rendered by ScarletUI rather than a mockup.
//! Run with KUREHA_WORKBENCH_PREVIEW=artifacts/bubble-glass and
//! `cargo test --bin kureha-code workbench_preview -- --ignored --test-threads=1`.
use super::*;
use kureha_code::{syntax, theme::Theme};
use scarlet_ui::pipeline::RenderingPipeline;
use std::{
    io::Write,
    time::{Duration, Instant},
};

#[test]
#[ignore = "manual visual review; writes PPM files to KUREHA_WORKBENCH_PREVIEW"]
fn workbench_preview() {
    let directory =
        PathBuf::from(std::env::var_os("KUREHA_WORKBENCH_PREVIEW").expect("preview directory"));
    std::fs::create_dir_all(&directory).unwrap();
    let themes = Theme::ALL
        .into_iter()
        .chain(
            kureha_code::theme::PRESETS
                .iter()
                .map(|(id, _)| kureha_code::theme::preset(id).unwrap()),
        )
        .chain([kureha_code::theme::parse_theme(
            "name='Code Colors'\nbase='light'\n[code]\ntheme='tokyo-night'\nfont_size=18",
        )
        .unwrap()]);
    for theme in themes {
        for scene in ["empty", "workbench", "find", "palette", "search", "themes"] {
            if !matches!(theme, Theme::Light | Theme::Dark)
                && scene != "workbench"
                && !(theme.name() == "Kureha" && scene == "themes")
            {
                continue;
            }
            let app = if scene == "empty" {
                App::new()
            } else {
                App::with_document()
            };
            app.set_theme(theme.clone());
            if scene != "empty" {
                app.runtime.lock().unwrap().explorer.root = Some("/project/kureha-code".into());
                app.tree_rows.set(
                    [
                        ("kureha-code", true, 0),
                        ("assets", true, 1),
                        ("src", true, 1),
                        ("editor.rs", false, 2),
                        ("explorer.rs", false, 2),
                        ("hover.rs", false, 2),
                        ("main.rs", false, 2),
                        ("theme.rs", false, 2),
                        ("ui.rs", false, 2),
                        ("Cargo.lock", false, 1),
                        ("Cargo.toml", false, 1),
                        ("flake.nix", false, 1),
                        ("README.md", false, 1),
                    ]
                    .into_iter()
                    .map(|(name, directory, depth)| Row {
                        key: name.into(),
                        label: name.into(),
                        path: Some(PathBuf::from("/project/kureha-code").join(name)),
                        directory,
                        depth,
                        expanded: directory && name != "assets",
                    })
                    .collect(),
                );
                app.tree_selected.set(Some(7));
                let path = PathBuf::from("/project/kureha-code/src/theme.rs");
                let id = app
                    .runtime
                    .lock()
                    .unwrap()
                    .workspace
                    .open(path.clone(), include_str!("theme.rs"));
                app.activate(id);
                app.language_status.set("Rust".into());
                let _ = app.content(); // Schedule syntax work, then wait for that exact revision.
                let deadline = Instant::now() + Duration::from_secs(5);
                loop {
                    syntax::poll();
                    if syntax::cached(id, 0, Some(&path))
                        .is_some_and(|snapshot| !snapshot.spans().is_empty())
                    {
                        break;
                    }
                    assert!(Instant::now() < deadline, "syntax fixture timed out");
                    thread::sleep(Duration::from_millis(5));
                }
            }
            match scene {
                "find" => app.show_find(),
                "palette" => {
                    app.show_commands();
                    app.poll_navigation();
                }
                "themes" => app.show_theme_picker(false),
                "search" => app.show_project_search(),
                _ => {}
            }
            let mut pipeline = RenderingPipeline::new();
            pipeline.set_root(
                Window::new("Kureha Code", app.content())
                    .decorated(false)
                    .size(Size::new(1280.0, 800.0))
                    .create_element(),
            );
            pipeline.layout_initial();
            let buffer = pipeline.render().expect("workbench paints");
            let mut file = std::io::BufWriter::new(
                std::fs::File::create(
                    directory.join(format!("{scene}-{}.ppm", theme.name().to_lowercase())),
                )
                .unwrap(),
            );
            write!(file, "P6\n{} {}\n255\n", buffer.width(), buffer.height()).unwrap();
            for pixel in buffer.data().chunks_exact(4) {
                file.write_all(&[pixel[2], pixel[1], pixel[0]]).unwrap();
            }
            pipeline.teardown();
        }
    }
}

#[test]
#[ignore = "manual native-renderer review; writes terminal layout PPMs"]
fn terminal_layout_preview() {
    let directory =
        PathBuf::from(std::env::var_os("KUREHA_WORKBENCH_PREVIEW").expect("preview directory"));
    std::fs::create_dir_all(&directory).unwrap();
    let app = App::with_document();
    app.apply_theme(kureha_code::theme::preset("kureha").unwrap());
    let first = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .open(PathBuf::from("src/main.rs"), include_str!("main.rs"));
    app.activate(first);
    app.split_editor(false);
    let second = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .open(PathBuf::from("src/groups.rs"), include_str!("groups.rs"));
    app.activate(second);
    app.new_terminal();
    let terminal = app.terminal.active().unwrap().terminal;
    let deadline = Instant::now() + Duration::from_secs(8);
    while terminal.status() != kureha_code::terminal::TerminalStatus::Running {
        terminal.poll();
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(10));
    }
    terminal.send_input(b"exec /bin/sh -i\r");
    terminal.send_input(b"stty -echo; printf '\\033[2J\\033[H\\033[38;5;211mKureha Code\\033[0m\\n\\n$ cargo test\\n\\n\\033[32mAll checks passed.\\033[0m\\n\\nSession stays alive when hidden or moved.\\nReady.\\n'\r");
    let mut pipeline = RenderingPipeline::new();
    for scene in [
        "terminal-bottom",
        "terminal-three-columns",
        "terminal-hidden",
        "terminal-restored",
    ] {
        match scene {
            "terminal-three-columns" => {
                let id = app.terminal.active().unwrap().id;
                app.new_terminal();
                app.split_terminal_from_panel(id, None, tab_drag::Edge::Right);
            }
            "terminal-hidden" => app.hide_panel(),
            "terminal-restored" => app.toggle_panel(),
            _ => {}
        }
        let window = || {
            Window::new("Kureha", app.content())
                .decorated(false)
                .size(Size::new(1440.0, 900.0))
        };
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        let deadline = Instant::now() + Duration::from_secs(2);
        while Instant::now() < deadline {
            app.poll_terminals();
            syntax::poll();
            pipeline
                .element_tree_mut()
                .root_mut()
                .unwrap()
                .update(&window());
            pipeline.render();
            thread::sleep(Duration::from_millis(20));
        }
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        let buffer = pipeline.render().expect("terminal layout paints");
        let mut file = std::io::BufWriter::new(
            std::fs::File::create(directory.join(format!("{scene}.ppm"))).unwrap(),
        );
        write!(file, "P6\n{} {}\n255\n", buffer.width(), buffer.height()).unwrap();
        for pixel in buffer.data().chunks_exact(4) {
            file.write_all(&[pixel[2], pixel[1], pixel[0]]).unwrap();
        }
    }
    pipeline.teardown();
    for session in app.terminal.sessions() {
        session.terminal.close();
    }
}

#[test]
#[ignore = "manual outer-edge layout review"]
fn outer_layout_preview() {
    let directory =
        PathBuf::from(std::env::var_os("KUREHA_WORKBENCH_PREVIEW").expect("preview directory"));
    std::fs::create_dir_all(&directory).unwrap();
    let app = App::with_document();
    app.apply_theme(kureha_code::theme::preset("kureha").unwrap());
    let first = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .open(PathBuf::from("src/main.rs"), include_str!("main.rs"));
    app.activate(first);
    app.split_editor(true);
    let id = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .open(PathBuf::from("src/groups.rs"), include_str!("groups.rs"));
    app.activate(id);
    app.split_tab_outer(id, 1, tab_drag::Edge::Right);
    let mut pipeline = RenderingPipeline::new();
    pipeline.set_root(
        Window::new("Outer", app.content())
            .decorated(false)
            .size(Size::new(1440.0, 900.0))
            .create_element(),
    );
    pipeline.layout_initial();
    let buffer = pipeline.render().unwrap();
    let mut file = std::io::BufWriter::new(
        std::fs::File::create(directory.join("editor-outer-right.ppm")).unwrap(),
    );
    write!(file, "P6\n{} {}\n255\n", buffer.width(), buffer.height()).unwrap();
    for pixel in buffer.data().chunks_exact(4) {
        file.write_all(&[pixel[2], pixel[1], pixel[0]]).unwrap();
    }
    pipeline.teardown();
}
