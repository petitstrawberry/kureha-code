use super::*;
use scarlet_ui::{Size, Window, pipeline::RenderingPipeline};

fn pixels(pipeline: &mut RenderingPipeline) -> Vec<u32> {
    pipeline
        .render()
        .expect("terminal frame")
        .as_slice()
        .to_vec()
}

#[test]
fn terminal_cells_and_pty_size_use_the_same_font_metrics() {
    let font = crate::fonts::editor_stack().unwrap();
    let cell = view::cell_size(Some(&font));
    assert!(
        cell.width
            >= scarlet_ui::graphics::measure_text_sized_with_font_stack("W", 14.0, &font).0 as f32
    );
    assert!(
        cell.height >= scarlet_ui::graphics::line_height_sized_with_font_stack(14.0, &font) as f32
    );
    let terminal = Terminal::new();
    let mut element = terminal.view_with_font(Some(font)).create_element();
    element.layout(scarlet_ui::LayoutConstraints::tight(
        cell.width * 90.0,
        cell.height * 12.0,
    ));
    assert_eq!(*terminal.shared.size.lock().unwrap(), (90, 12));
}

#[test]
fn cursor_only_output_and_focus_changes_repaint_without_a_workbench_rebuild() {
    for paint_enabled in [false, true] {
        let terminal = Terminal::new();
        terminal.focus();
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(paint_enabled);
        pipeline.set_root(
            Window::new("Terminal", terminal.view())
                .decorated(false)
                .size(Size::new(160.0, 68.0))
                .create_element(),
        );
        pipeline.layout_initial();
        let initial = pixels(&mut pipeline);

        // No screen text or parent state changes: ANSI cursor motion must be
        // visible on the very next frame, not on an unrelated editor event.
        terminal.shared.cursor.set(TextGridCursor::new(4, 1));
        assert!(
            pipeline.has_dirty(),
            "cursor did not invalidate the terminal"
        );
        let moved = pixels(&mut pipeline);
        assert_ne!(moved, initial);

        terminal.shared.cursor.set(TextGridCursor {
            visible: false,
            ..TextGridCursor::new(4, 1)
        });
        let hidden = pixels(&mut pipeline);
        assert_ne!(hidden, moved);
        terminal.shared.cursor.set(TextGridCursor::new(4, 1));
        assert_eq!(pixels(&mut pipeline), moved);
        terminal.blur();
        assert_eq!(pixels(&mut pipeline), hidden);
        terminal.focus();
        assert_eq!(pixels(&mut pipeline), moved);

        terminal.shared.cursor.set(TextGridCursor::new(0, 0));
        assert_eq!(
            pixels(&mut pipeline),
            initial,
            "old cursor left stale pixels"
        );
        pipeline.teardown();
    }
}

#[test]
fn palette_switch_recolors_existing_and_pending_output_and_repaints_idle_terminal() {
    let terminal = Terminal::new();
    let mut parser = vt100::Parser::new(4, 24, 8);
    // Explicit RGB identical to the old ANSI red must remain explicit RGB.
    parser.process(b"D\x1b[31mR\x1b[91mB\x1b[38;2;205;49;49mT\x1b[38;5;196mX\x1b[0;7;4;1mI\x1b[0m");
    let pending = screen::snapshot(parser.screen(), TerminalStatus::Running);
    terminal.apply_snapshot(screen::snapshot(parser.screen(), TerminalStatus::Running));
    terminal.focus();
    let generation = terminal.generation();
    let cursor = terminal.shared.cursor.get();
    let mut pipeline = RenderingPipeline::new();
    pipeline.set_root(
        Window::new("Terminal", terminal.view())
            .decorated(false)
            .size(Size::new(224.0, 102.0))
            .create_element(),
    );
    pipeline.layout_initial();
    let old = pixels(&mut pipeline);
    let light = crate::theme::preset("kureha").unwrap().terminal_palette();
    terminal.set_palette(light);
    assert!(pipeline.has_dirty());
    let pink = pixels(&mut pipeline);
    assert_ne!(pink, old);
    let assert_colors = |palette: TerminalPalette| {
        let grid = terminal.grid();
        assert_eq!(grid.cell(0, 0).unwrap().foreground, palette.foreground);
        assert_eq!(grid.cell(1, 0).unwrap().foreground, palette.ansi[1]);
        assert_eq!(grid.cell(2, 0).unwrap().foreground, palette.ansi[9]);
        assert_eq!(
            grid.cell(3, 0).unwrap().foreground,
            scarlet_ui::Color::rgb(205, 49, 49)
        );
        assert_eq!(
            grid.cell(4, 0).unwrap().foreground,
            scarlet_ui::Color::rgb(255, 0, 0)
        );
        let inverse = grid.cell(5, 0).unwrap();
        assert!(inverse.inverse && inverse.underline && inverse.bold);
        assert_eq!(grid.cell(23, 3).unwrap().background, palette.background);
    };
    assert_colors(light);
    terminal.apply_snapshot(pending);
    assert_colors(light);
    assert_eq!(pixels(&mut pipeline), pink);
    let dark = crate::theme::preset("monokai").unwrap().terminal_palette();
    terminal.set_palette(dark);
    assert_colors(dark);
    assert_ne!(pixels(&mut pipeline), pink);
    terminal.set_palette(light);
    assert_eq!(pixels(&mut pipeline), pink);
    terminal.set_palette(light);
    assert!(
        !pipeline.has_dirty(),
        "same palette should not schedule another frame"
    );
    assert_eq!(terminal.generation(), generation);
    assert_eq!(terminal.shared.cursor.get(), cursor);
    assert_eq!(terminal.grid().cell(0, 0).unwrap().ch, 'D');
    pipeline.teardown();
}

#[test]
#[ignore = "manual palette review; writes KUREHA_TERMINAL_PREVIEW PNG"]
fn terminal_palette_screenshot() {
    use scarlet_ui::{Color, Text, VStack, ViewExt};
    let mut parser = vt100::Parser::new(8, 64, 0);
    parser.process(b"kureha-code ~/projects/kureha\r\n\r\n$ cargo check\r\n\x1b[32m    Finished\x1b[0m dev profile in 0.42s\r\n\r\n");
    for i in 0..8 {
        parser.process(format!("\x1b[{}m  color", 30 + i).as_bytes());
    }
    parser.process(b"\x1b[0m\r\n$ ");
    let terminals: Vec<_> = ["kureha", "monokai", "tokyo-night"]
        .into_iter()
        .map(|id| {
            let theme = crate::theme::preset(id).unwrap();
            let terminal = Terminal::new();
            terminal.apply_snapshot(screen::snapshot(parser.screen(), TerminalStatus::Running));
            terminal.set_palette(theme.terminal_palette());
            terminal.focus();
            VStack::new((
                Text::new(theme.name())
                    .color(theme.colors().foreground)
                    .font_size(16.0),
                terminal
                    .view_with_font(crate::fonts::editor_stack())
                    .frame(600.0, 170.0),
            ))
            .spacing(12.0)
            .padding(16.0)
            .background(theme.terminal_palette().background)
        })
        .collect();
    let mut pipeline = RenderingPipeline::new();
    pipeline.set_root(
        Window::new(
            "Palettes",
            VStack::new((
                terminals[0].clone(),
                terminals[1].clone(),
                terminals[2].clone(),
            ))
            .spacing(12.0)
            .padding(12.0)
            .background(Color::rgb(232, 220, 226)),
        )
        .decorated(false)
        .size(Size::new(656.0, 744.0))
        .create_element(),
    );
    pipeline.layout_initial();
    let buffer = pipeline.render().unwrap();
    let rgba: Vec<_> = buffer
        .as_slice()
        .iter()
        .flat_map(|pixel| {
            [
                (pixel >> 16) as u8,
                (pixel >> 8) as u8,
                *pixel as u8,
                (pixel >> 24) as u8,
            ]
        })
        .collect();
    image::save_buffer(
        std::env::var_os("KUREHA_TERMINAL_PREVIEW").unwrap(),
        &rgba,
        buffer.width(),
        buffer.height(),
        image::ColorType::Rgba8,
    )
    .unwrap();
    pipeline.teardown();
}

#[test]
fn tab_and_shift_tab_reach_the_pty_queue_and_keep_keyboard_focus() {
    use scarlet_ui::{Button, VStack, event::KeyModifiers};
    let terminal = Terminal::new();
    let inbox = Arc::new(Mutex::new(Inbox::default()));
    terminal.shared.status.set(TerminalStatus::Running);
    *terminal.shared.session.lock().unwrap() = Some(Session {
        cancel: Arc::new(AtomicBool::new(false)),
        inbox: inbox.clone(),
        output: Arc::new(Mutex::new(None)),
    });
    terminal.focus();
    let mut pipeline = RenderingPipeline::new();
    pipeline.set_root(
        Window::new(
            "Terminal input",
            VStack::new((Button::new("Before"), terminal.view(), Button::new("After"))),
        )
        .decorated(false)
        .size(Size::new(480.0, 240.0))
        .create_element(),
    );
    pipeline.layout_initial();
    pipeline.render();
    for shift in [false, true, false] {
        assert!(pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Tab,
            modifiers: KeyModifiers {
                shift,
                ..KeyModifiers::empty()
            }
        })));
        pipeline.render();
        assert!(terminal.focused());
    }
    assert_eq!(
        inbox
            .lock()
            .unwrap()
            .input
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        b"\t\x1b[Z\t"
    );
    pipeline.teardown();
    terminal.close();
}
