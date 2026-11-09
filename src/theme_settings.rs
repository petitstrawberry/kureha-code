//! Theme I/O and parsing stay off the event loop. Invalid saves retain the last palette.
use kureha_code::theme::{THEME_TEMPLATE, Theme, parse_theme};
use std::{
    fs::{self, OpenOptions},
    io::{Read, Write},
    path::PathBuf,
    sync::{Mutex, mpsc},
    thread,
    time::Duration,
};

pub(super) enum Update {
    Loaded(Theme),
    Open(PathBuf),
    Error(String),
}
enum Command {
    Reload,
    Open,
}
pub(super) struct Watcher {
    commands: mpsc::Sender<Command>,
    updates: Mutex<mpsc::Receiver<Update>>,
}
impl Watcher {
    pub fn start(path: PathBuf) -> std::io::Result<Self> {
        let (commands, receiver) = mpsc::channel();
        let (sender, updates) = mpsc::channel();
        thread::Builder::new()
            .name("kureha-theme".into())
            .spawn(move || {
                let mut previous = None;
                let mut force = false;
                loop {
                    let result = read(&path);
                    if force || previous.as_ref() != Some(&result) {
                        force = false;
                        let update = match &result {
                            Ok(Some(source)) => Some(
                                match parse_theme(source).and_then(|theme| {
                                    theme.prepare_font(
                                        path.parent().unwrap_or(std::path::Path::new(".")),
                                    )
                                }) {
                                    Ok(theme) => Update::Loaded(theme),
                                    Err(e) => Update::Error(format!("Theme: {e}")),
                                },
                            ),
                            Ok(None) => {
                                if previous.is_some() {
                                    Some(Update::Error(
                                        "Theme file missing; keeping current theme".into(),
                                    ))
                                } else {
                                    None
                                }
                            }
                            Err(e) => Some(Update::Error(format!("Theme: {e}"))),
                        };
                        previous = Some(result);
                        if let Some(update) = update {
                            if sender.send(update).is_err() {
                                break;
                            }
                        }
                    }
                    match receiver.recv_timeout(Duration::from_millis(500)) {
                        Ok(Command::Reload) => force = true,
                        Ok(Command::Open) => {
                            let result = create(&path)
                                .map(|()| Update::Open(path.clone()))
                                .unwrap_or_else(|e| Update::Error(format!("Theme: {e}")));
                            if sender.send(result).is_err() {
                                break;
                            }
                        }
                        Err(mpsc::RecvTimeoutError::Timeout) => {}
                        Err(mpsc::RecvTimeoutError::Disconnected) => break,
                    }
                }
            })?;
        Ok(Self {
            commands,
            updates: Mutex::new(updates),
        })
    }
    pub fn open(&self) {
        let _ = self.commands.send(Command::Open);
    }
    pub fn reload(&self) {
        let _ = self.commands.send(Command::Reload);
    }
    pub fn poll(&self) -> Vec<Update> {
        self.updates.lock().unwrap().try_iter().collect()
    }
}
fn read(path: &std::path::Path) -> Result<Option<String>, String> {
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(64 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() > 64 * 1024 {
        return Err("Theme exceeds 64 KiB".into());
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|e| e.to_string())
}
fn create(path: &std::path::Path) -> std::io::Result<()> {
    if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
        fs::create_dir_all(parent)?;
    }
    match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(mut file) => file.write_all(THEME_TEMPLATE.as_bytes()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(()),
        Err(e) => Err(e),
    }
}

impl super::App {
    pub(super) fn start_theme_settings(&self) {
        let mut watcher = self.theme_settings.lock().unwrap();
        if watcher.is_some() {
            return;
        }
        let Some(path) = kureha_code::theme::theme_path() else {
            self.status
                .set("Cannot locate theme file; set KUREHA_THEME".into());
            return;
        };
        match Watcher::start(path) {
            Ok(value) => *watcher = Some(value),
            Err(e) => self.status.set(format!("Cannot watch theme: {e}")),
        }
    }
    pub(super) fn open_theme_settings(&self) {
        self.start_theme_settings();
        if let Some(watcher) = self.theme_settings.lock().unwrap().as_ref() {
            watcher.open();
        }
    }
    pub(super) fn reload_theme_settings(&self) {
        self.start_theme_settings();
        if let Some(watcher) = self.theme_settings.lock().unwrap().as_ref() {
            watcher.reload();
        }
    }
    pub(super) fn poll_theme_settings(&self) {
        let updates = self
            .theme_settings
            .lock()
            .unwrap()
            .as_ref()
            .map(Watcher::poll)
            .unwrap_or_default();
        for update in updates {
            match update {
                Update::Loaded(theme) => {
                    let name = theme.name().to_owned();
                    self.set_theme(theme);
                    self.status.set(format!("Theme: {name}"));
                }
                Update::Open(path) => self.open(path),
                Update::Error(error) => self.status.set(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn watcher_recovers_from_invalid_and_atomic_saves_without_overwriting_settings() {
        let dir = std::env::temp_dir().join(format!("kureha-theme-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("theme.toml");
        fs::write(&path, "name='First'\nbase='light'").unwrap();
        let watcher = Watcher::start(path.clone()).unwrap();
        fn next(w: &Watcher) -> Update {
            w.updates
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(5))
                .unwrap()
        }
        assert!(matches!(next(&watcher), Update::Loaded(t) if t.name() == "First"));
        fs::write(&path, "[broken").unwrap();
        watcher.reload();
        assert!(matches!(next(&watcher), Update::Error(_)));
        fs::write(
            dir.join("next"),
            "name='Second'\n[colors]\naccent='#123456'",
        )
        .unwrap();
        fs::rename(dir.join("next"), &path).unwrap();
        // No explicit reload: saving a replacement file must be observed.
        assert!(matches!(next(&watcher), Update::Loaded(t) if t.name() == "Second"));
        create(&path).unwrap();
        assert!(fs::read_to_string(&path).unwrap().contains("Second"));
        drop(watcher);
        fs::remove_dir_all(dir).unwrap();
    }
}
