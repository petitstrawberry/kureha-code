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
    for theme in Theme::ALL {
        for scene in ["empty", "workbench", "find", "palette", "search"] {
            let app = App::new();
            app.set_theme(theme);
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
