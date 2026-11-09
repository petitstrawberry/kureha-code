//! macOS keeps native traffic lights while drawing the title strip as part of
//! the workbench. Only this empty strip initiates a window drag.
use super::*;
use scarlet_ui::event::{MouseButton, MouseEvent};

impl App {
    pub(super) fn window_header(&self) -> impl View + Clone + use<> {
        let app = self.clone();
        let theme = self.theme.get();
        if_view!(
            cfg!(target_os = "macos"),
            hstack! {
                Spacer::new(),
                Text::new(self.window_title()).font_size(12.0).color(theme.colors().muted).clip(),
                Spacer::new(),
            }
            .spacing(0.0)
            .padding_insets(EdgeInsets::new(90.0, 0.0, 90.0, 0.0))
            .frame(f32::INFINITY, 32.0)
            .background(theme.workspace_background())
            .on_event(move |event| {
                if matches!(
                    event,
                    Event::Mouse(MouseEvent::ButtonPressed {
                        button: MouseButton::Left,
                        ..
                    })
                ) {
                    app.runtime.lock().unwrap().window_drag_requested = true;
                    app.changed();
                    return true;
                }
                false
            }),
            VStack::new(())
        )
    }
}
