//! Messages for the status bar: what just happened (shown for a few seconds;
//! errors until the next message or a click), work in progress, and a
//! history the palette can show.
//!
//! Code that writes `App::status` directly still works: every change is
//! recorded as an information message. New code calls [`App::notify`] with a
//! level, and [`App::progress`] for work that takes a while.
use super::*;
use std::{collections::VecDeque, time::Instant};

/// How long an information message stays in the status bar.
const SHOWN_FOR: std::time::Duration = std::time::Duration::from_secs(8);
/// Messages kept for the history.
const KEPT: usize = 100;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Level {
    Info,
    /// Stays until the next message or a click.
    Error,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Message {
    pub text: String,
    pub level: Level,
    pub at: Instant,
}

#[derive(Clone)]
pub(super) struct Messages {
    /// The message in the status bar, if any.
    pub(super) shown: State<Option<Message>>,
    /// Work in progress, oldest first: (id, what it is doing).
    pub(super) progress: State<Vec<(u64, String)>>,
    history: Arc<Mutex<VecDeque<Message>>>,
    next: Arc<AtomicU64>,
    /// Set while `notify` writes `App::status`, so it is recorded once.
    writing: Arc<std::sync::atomic::AtomicBool>,
}
impl Messages {
    pub(super) fn new() -> Self {
        Self {
            shown: State::new(generate_state_id(), None),
            progress: State::new(generate_state_id(), Vec::new()),
            history: Default::default(),
            next: Default::default(),
            writing: Default::default(),
        }
    }
    pub(super) fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![&self.shown, &self.progress]
    }
    fn record(&self, text: String, level: Level) {
        if text.is_empty() {
            if self.shown.get().is_some() {
                self.shown.set(None);
            }
            return;
        }
        let message = Message {
            text,
            level,
            at: Instant::now(),
        };
        let mut history = self.history.lock().unwrap();
        if history.len() == KEPT {
            history.pop_front();
        }
        history.push_back(message.clone());
        drop(history);
        self.shown.set(Some(message));
    }
    /// Newest first.
    pub(super) fn history(&self) -> Vec<Message> {
        self.history.lock().unwrap().iter().rev().cloned().collect()
    }
    /// Mirrors writes to `status` (the older way to report) as messages.
    pub(super) fn follow(&self, status: &State<String>) {
        let messages = self.clone();
        let source = status.clone();
        status.subscribe_any(Arc::new(move || {
            if !messages.writing.load(Ordering::Relaxed) {
                messages.record(source.get(), Level::Info);
            }
        }));
    }
}

/// How long ago `at` was, briefly.
pub(super) fn ago(at: Instant) -> String {
    match at.elapsed().as_secs() {
        0..=4 => "just now".into(),
        s @ 5..=59 => format!("{s} s ago"),
        s @ 60..=3599 => format!("{} min ago", s / 60),
        s => format!("{} h ago", s / 3600),
    }
}

impl App {
    /// Reports something that happened.
    pub(super) fn notify(&self, level: Level, text: impl Into<String>) {
        let text = text.into();
        self.messages.writing.store(true, Ordering::Relaxed);
        self.status.set(text.clone());
        self.messages.writing.store(false, Ordering::Relaxed);
        self.messages.record(text, level);
    }
    /// Shows `text` in the status bar until [`Self::progress_done`].
    pub(super) fn progress(&self, text: impl Into<String>) -> u64 {
        let id = self.messages.next.fetch_add(1, Ordering::Relaxed);
        let text = text.into();
        self.messages
            .progress
            .update(|items| items.push((id, text)));
        id
    }
    pub(super) fn progress_done(&self, id: u64) {
        if self
            .messages
            .progress
            .get()
            .iter()
            .any(|(item, _)| *item == id)
        {
            self.messages
                .progress
                .update(|items| items.retain(|(item, _)| *item != id));
        }
    }
    /// Hides an information message once it has been shown long enough.
    pub(super) fn expire_messages(&self) {
        if let Some(message) = self.messages.shown.get()
            && message.level == Level::Info
            && message.at.elapsed() >= SHOWN_FOR
        {
            self.messages.shown.set(None);
        }
    }

    /// The left of the status bar: Git, work in progress, the latest message.
    pub(super) fn status_bar_left(&self) -> impl View + Clone + use<> {
        let colors = self.theme.get().colors();
        let progress: Vec<String> = self
            .messages
            .progress
            .get()
            .into_iter()
            .map(|(_, text)| text)
            .collect();
        let shown = self.messages.shown.get();
        let history = self.clone();
        let (text, color) = match &shown {
            Some(message) if message.level == Level::Error => (message.text.clone(), colors.error),
            Some(message) => (message.text.clone(), colors.foreground),
            None => (String::new(), colors.muted),
        };
        hstack! {
            self.git_status_bar(),
            HStack::new(super::ui::ViewCollection(
                progress
                    .into_iter()
                    .map(|text| Text::new(text).font_size(11.0).color(colors.muted))
                    .collect(),
            ))
            .spacing(10.0),
            if_view!(!text.is_empty(),
                Text::new(text.clone()).font_size(11.0).color(color)
                    .on_event(move |event| {
                        if matches!(
                            event,
                            Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed {
                                button: scarlet_ui::event::MouseButton::Left,
                                ..
                            })
                        ) {
                            history.show_message_history();
                            return true;
                        }
                        false
                    }),
                VStack::new(())),
        }
        .spacing(12.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn messages_come_and_go_and_stay_in_the_history() {
        let app = App::new();
        assert_eq!(
            app.messages.shown.get(),
            None,
            "nothing like Ready at start"
        );
        // The older way to report still reaches the status bar and history.
        app.status.set("Saved a.txt".into());
        let shown = app.messages.shown.get().unwrap();
        assert_eq!(
            (shown.text.as_str(), shown.level),
            ("Saved a.txt", Level::Info)
        );
        // Information goes away after a while; an error stays.
        app.messages
            .shown
            .update(|m| m.as_mut().unwrap().at -= SHOWN_FOR);
        app.expire_messages();
        assert_eq!(app.messages.shown.get(), None);
        app.notify(Level::Error, "Cannot write b.txt");
        assert_eq!(app.status.get(), "Cannot write b.txt");
        app.messages
            .shown
            .update(|m| m.as_mut().unwrap().at -= SHOWN_FOR);
        app.expire_messages();
        assert_eq!(app.messages.shown.get().unwrap().level, Level::Error);
        // Clearing the status (opening a file) clears the message.
        app.status.set(String::new());
        assert_eq!(app.messages.shown.get(), None);
        let history: Vec<_> = app.messages.history().into_iter().map(|m| m.text).collect();
        assert_eq!(
            history,
            ["Cannot write b.txt", "Saved a.txt"],
            "newest first, once each"
        );

        let first = app.progress("Switching to side…");
        let second = app.progress("Formatting…");
        app.progress_done(first);
        assert_eq!(
            app.messages.progress.get(),
            [(second, "Formatting…".to_string())]
        );
        app.progress_done(second);
        assert!(app.messages.progress.get().is_empty());

        app.notify(Level::Error, "Again");
        app.show_message_history();
        assert_eq!(
            app.messages.shown.get(),
            None,
            "reading the history dismisses it"
        );
    }
}
