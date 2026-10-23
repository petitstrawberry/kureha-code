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
