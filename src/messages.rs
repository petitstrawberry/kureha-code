//! Messages for the status bar: what just happened (shown for a few seconds;
//! errors until the next message or a click), work in progress, and a
//! history the palette can show. Questions that need an answer (allowing an
//! extension, say) are prompts with buttons, shown above the status bar.
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

/// A question with buttons, shown above the status bar until it is answered
/// or what it asks about no longer applies.
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Prompt {
    /// What it is about. One prompt per key: asking again replaces it.
    pub key: String,
    pub title: String,
    pub detail: String,
    /// Button labels, the main one first.
    pub choices: Vec<String>,
}

/// What a prompt does with the chosen button's index.
type Answer = Arc<dyn Fn(&App, usize) + Send + Sync>;

/// Prompts shown at once; the rest wait for those to go.
const PROMPTS_SHOWN: usize = 3;
/// How far above the window's bottom edge prompts sit: inside the editor's
/// corner, clear of the status bar.
const PROMPT_INSET: f32 = 44.0;

#[derive(Clone)]
pub(super) struct Messages {
    /// The message in the status bar, if any.
    pub(super) shown: State<Option<Message>>,
    /// Work in progress, oldest first: (id, what it is doing).
    pub(super) progress: State<Vec<(u64, String)>>,
    /// Questions waiting for an answer, oldest first.
    pub(super) prompts: State<Vec<Prompt>>,
    answers: Arc<Mutex<std::collections::HashMap<String, Answer>>>,
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
            prompts: State::new(generate_state_id(), Vec::new()),
            answers: Default::default(),
            history: Default::default(),
            next: Default::default(),
            writing: Default::default(),
        }
    }

    pub(super) fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![&self.shown, &self.progress, &self.prompts]
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

    /// Shows `prompt` until a button is clicked, which calls `answer` with its
    /// index, or until [`Self::withdraw`]. Asking the same again changes
    /// nothing; asking with the same key replaces the earlier one.
    pub(super) fn ask(&self, prompt: Prompt, answer: impl Fn(&App, usize) + Send + Sync + 'static) {
        let key = prompt.key.clone();
        self.messages
            .answers
            .lock()
            .unwrap()
            .insert(key.clone(), Arc::new(answer));
        if self.messages.prompts.get().contains(&prompt) {
            return;
        }
        self.messages.prompts.update(|prompts| {
            prompts.retain(|p| p.key != key);
            prompts.push(prompt);
        });
    }

    /// Removes the prompt with `key`, if it is shown.
    pub(super) fn withdraw(&self, key: &str) {
        self.messages.answers.lock().unwrap().remove(key);
        if self.messages.prompts.get().iter().any(|p| p.key == key) {
            self.messages
                .prompts
                .update(|prompts| prompts.retain(|p| p.key != key));
        }
    }

    /// Clicks button `choice` of the prompt with `key`.
    pub(super) fn answer(&self, key: &str, choice: usize) {
        let answer = self.messages.answers.lock().unwrap().remove(key);
        self.withdraw(key);
        if let Some(answer) = answer {
            answer(self, choice);
        }
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
    /// The prompts, floating over the bottom-right corner of the workbench
    /// above the status bar, so the layout does not move; nothing when there
    /// are none.
    pub(super) fn prompt_overlay(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let colors = theme.colors();
        let prompts = self.messages.prompts.get();
        let cards = prompts
            .iter()
            .take(PROMPTS_SHOWN)
            .map(|prompt| {
                let buttons = prompt
                    .choices
                    .iter()
                    .enumerate()
                    .map(|(index, label)| {
                        let (app, key) = (self.clone(), prompt.key.clone());
                        // The main choice in the accent colour.
                        let (text, border) = if index == 0 {
                            (colors.accent, colors.accent)
                        } else {
                            (colors.foreground, colors.border)
                        };
                        Button::new(label.clone())
                            .font_size(12.0)
                            .text_color(text)
                            .border_color(border)
                            .material(theme.input_material())
                            .on_click(move || app.answer(&key, index))
                    })
                    .collect();
                hstack! {
                    vstack! {
                        Text::new(prompt.title.clone()).font_size(13.0).color(colors.foreground).line_limit(Some(2)),
                        if_view!(!prompt.detail.is_empty(),
                            Text::new(prompt.detail.clone()).font_size(11.0).color(colors.muted).line_limit(Some(3)),
                            VStack::new(())),
                    }
                    .alignment(Alignment::Leading)
                    .spacing(4.0)
                    .frame_width(f32::INFINITY),
                    HStack::new(super::ui::ViewCollection(buttons)).spacing(8.0),
                }
                .spacing(16.0)
                .padding_insets(EdgeInsets::new(14.0, 10.0, 12.0, 10.0))
                .surface_style(theme.popup_style())
                .corner_radius(10.0)
            })
            .collect::<Vec<_>>();
        if_view!(
            !cards.is_empty(),
            super::popup::Anchored::bottom_right(
                VStack::new(super::ui::ViewCollection(cards))
                    .spacing(8.0)
                    .padding_insets(EdgeInsets::new(0.0, 0.0, 10.0, 0.0)),
                PROMPT_INSET,
            ),
            VStack::new(())
        )
    }

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

    #[test]
    fn prompts_wait_for_an_answer_and_one_answer_ends_them() {
        let app = App::new();
        let asked = |key: &str, title: &str| Prompt {
            key: key.into(),
            title: title.into(),
            detail: String::new(),
            choices: vec!["Allow".into(), "Not now".into()],
        };
        let answers = Arc::new(Mutex::new(Vec::new()));
        let record = |answers: &Arc<Mutex<Vec<(String, usize)>>>, tag: &'static str| {
            let answers = answers.clone();
            move |_: &App, choice: usize| answers.lock().unwrap().push((tag.to_string(), choice))
        };
        app.ask(asked("a", "First"), record(&answers, "a"));
        app.ask(asked("b", "Second"), record(&answers, "b"));
        let changes = Arc::new(AtomicU64::new(0));
        let counted = changes.clone();
        app.messages.prompts.subscribe_any(Arc::new(move || {
            counted.fetch_add(1, Ordering::Relaxed);
        }));
        // The same question again changes nothing; the same key replaces it.
        app.ask(asked("a", "First"), record(&answers, "a"));
        assert_eq!(changes.load(Ordering::Relaxed), 0, "no repaint");
        app.ask(asked("a", "First, again"), record(&answers, "a2"));
        let titles = || {
            app.messages
                .prompts
                .get()
                .into_iter()
                .map(|p| p.title)
                .collect::<Vec<_>>()
        };
        assert_eq!(titles(), ["Second", "First, again"]);
        app.answer("a", 1);
        assert_eq!(*answers.lock().unwrap(), [("a2".to_string(), 1)]);
        assert_eq!(titles(), ["Second"]);
        app.answer("a", 0);
        assert_eq!(answers.lock().unwrap().len(), 1, "answered once");
        app.withdraw("b");
        app.answer("b", 0);
        assert!(titles().is_empty());
        assert_eq!(
            answers.lock().unwrap().len(),
            1,
            "withdrawn ones do nothing"
        );
        assert!(
            app.messages.history().is_empty(),
            "prompts are not messages"
        );
    }
}
