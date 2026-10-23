use super::*;
use scarlet_ui::LayoutConstraints;
use scarlet_ui::event::KeyModifiers;

fn wait_until(terminal: &Terminal, mut predicate: impl FnMut(&Terminal) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        terminal.poll();
        if predicate(terminal) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "terminal timeout: {:?}\n{}",
            terminal.status(),
            text(terminal)
        );
        thread::sleep(Duration::from_millis(10));
    }
}
fn text(terminal: &Terminal) -> String {
    let grid = terminal.grid();
    (0..grid.rows())
        .map(|row| {
            (0..grid.columns())
                .filter_map(|col| grid.cell(col, row))
                .map(|cell| cell.ch)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn ansi_cursor_color_unicode_alternate_screen_and_bounded_scrollback() {
    let mut parser = vt100::Parser::new(3, 12, 4);
    parser.process(b"abc\x1b[2D\x1b[31;1mZ\x1b[0m\r\n");
    for byte in "界".as_bytes() {
        parser.process(&[*byte]);
    }
    let snap = screen::snapshot(parser.screen(), TerminalStatus::Running);
    assert_eq!(snap.grid.cell(1, 0).unwrap().ch, 'Z');
    assert!(snap.grid.cell(1, 0).unwrap().bold);
    assert_eq!(
        snap.grid.cell(1, 0).unwrap().foreground,
        scarlet_ui::Color::rgb(205, 49, 49)
    );
    assert_eq!(snap.grid.cell(0, 1).unwrap().ch, '界');
    assert_eq!(snap.grid.cell(1, 1).unwrap().ch, '\0');
    parser.process(b"\x1b[?1049hALT\x1b[?1049l");
    assert!(parser.screen().contents().contains('Z'));
    for _ in 0..100 {
        parser.process(b"line\r\n");
    }
    parser.screen_mut().set_scrollback(4);
    assert!(parser.screen().scrollback() <= 4);
    parser.process(b"\x1b[?25l");
    assert!(
        !screen::snapshot(parser.screen(), TerminalStatus::Running)
            .cursor
            .visible
    );
}

#[test]
fn keys_control_sequences_modes_modifiers_and_paste_limit() {
    let pressed = |key, modifiers| KeyEvent::Pressed {
        keycode: key,
        modifiers,
    };
    let ctrl = KeyModifiers {
        control: true,
        ..Default::default()
    };
    let alt = KeyModifiers {
        alt: true,
        ..Default::default()
    };
    assert_eq!(
        encode_key(pressed(KeyCode::Char('c'), ctrl), false),
        Some(vec![3])
    );
    assert_eq!(
        encode_key(pressed(KeyCode::Char('d'), ctrl), false),
        Some(vec![4])
    );
    assert_eq!(
        encode_key(pressed(KeyCode::Up, Default::default()), true).unwrap(),
        b"\x1bOA"
    );
    assert_eq!(
        encode_key(pressed(KeyCode::Left, ctrl), false).unwrap(),
        b"\x1b[1;5D"
    );
    assert_eq!(
        encode_key(pressed(KeyCode::Char('x'), alt), false).unwrap(),
        b"\x1bx"
    );
    assert!(encode_key(pressed(KeyCode::Char('x'), Default::default()), false).is_none());
    assert_eq!(
        encode_key(KeyEvent::Char { c: '日' }, false).unwrap(),
        "日".as_bytes()
    );
    let terminal = Terminal::new();
    assert!(!terminal.paste(&"x".repeat(INPUT_LIMIT)));
    assert!(!terminal.send_input(b"closed"));
}

#[test]
fn query_replies_survive_chunk_boundaries_and_ignore_osc() {
    let mut parser = vt100::Parser::new(4, 20, 0);
    parser.process(b"\x1b[2;3H");
    let mut queries = screen::Queries::default();
    let mut replies = Vec::new();
    for byte in b"\x1b[6n\x1b]0;title\x1b[6n\x07\x1b[5n" {
        if let Some(reply) = queries.feed(*byte, parser.screen()) {
            replies.push(reply);
        }
    }
    assert_eq!(replies, vec![b"\x1b[2;3R".to_vec(), b"\x1b[0n".to_vec()]);
}

#[test]
fn measured_view_queues_actual_panel_size() {
    let terminal = Terminal::new();
    let mut element = terminal.view().create_element();
    element.layout(LayoutConstraints::tight(800.0, 170.0));
    assert_eq!(*terminal.shared.size.lock().unwrap(), (100, 10));
    terminal.resize(u16::MAX, u16::MAX);
    assert_eq!(*terminal.shared.size.lock().unwrap(), (300, 120));
}

#[test]
#[cfg(all(unix, not(target_os = "scarlet")))]
fn real_pty_interactive_cwd_input_ansi_resize_exit_and_close() {
    // Select a predictable real interactive shell without mutating process env.
    // The backend still launches $SHELL; `exec /bin/sh -i` happens inside its PTY.
    let cwd = std::env::temp_dir().join(format!("kureha-terminal-{}", std::process::id()));
    std::fs::create_dir_all(&cwd).unwrap();
    let terminal = Terminal::new();
    terminal.open(&cwd);
    wait_until(&terminal, |t| t.status() == TerminalStatus::Running);
    assert!(terminal.send_input(b"exec /bin/sh -i\r"));
    assert!(terminal.send_input(b"stty -echo; printf '\\033[2J\\033[H'; test -t 0 && printf 'REAL_%s\\n' TTY; pwd; printf '\\033[31mRED\\033[0m\\n'; printf 'RE%sDY\\n' A\r"));
    wait_until(&terminal, |t| {
        text(t).contains("READY")
            && text(t).contains("REAL_TTY")
            && text(t).contains(cwd.file_name().unwrap().to_str().unwrap())
    });
    assert!(text(&terminal).contains(cwd.file_name().unwrap().to_str().unwrap()));
    let grid = terminal.grid();
    assert!(
        grid.cells()
            .iter()
            .any(|cell| cell.ch == 'R' && cell.foreground == scarlet_ui::Color::rgb(205, 49, 49))
    );
    terminal.resize(93, 17);
    // Output confirms TIOCSWINSZ reached the actual TTY, not just the renderer.
    assert!(terminal.send_input(b"stty size; printf 'RESIZED\\n'\r"));
    wait_until(&terminal, |t| {
        text(t).contains("17 93") && text(t).contains("RESIZED")
    });
    assert!(terminal.send_input(b"read value; printf 'INPUT:%s\\n' \"$value\"\r"));
    assert!(terminal.send_input(b"roundtrip\r"));
    wait_until(&terminal, |t| text(t).contains("INPUT:roundtrip"));
    // Ctrl+C goes through the PTY line discipline to a foreground job, then the
    // interactive shell accepts the next command.
    assert!(terminal.send_input(b"sleep 30\r"));
    thread::sleep(Duration::from_millis(100));
    assert!(terminal.handle_key(KeyEvent::Pressed {
        keycode: KeyCode::Char('c'),
        modifiers: KeyModifiers {
            control: true,
            ..Default::default()
        }
    }));
    assert!(terminal.send_input(b"printf 'INTERRUPTED\\n'\r"));
    wait_until(&terminal, |t| text(t).contains("INTERRUPTED"));
    assert!(terminal.send_input(b"exit 7\r"));
    wait_until(&terminal, |t| {
        matches!(t.status(), TerminalStatus::Exited(Some(7)))
    });
    terminal.open(&cwd);
    wait_until(&terminal, |t| t.status() == TerminalStatus::Running);
    // A shell that ignores hangup must still be killed/reaped off the UI.
    assert!(terminal.send_input(b"trap '' HUP; echo $$\r"));
    let before = Instant::now();
    terminal.close();
    assert!(before.elapsed() < Duration::from_millis(100));
    assert_eq!(terminal.status(), TerminalStatus::Closed);
    std::fs::remove_dir(&cwd).unwrap();
}

#[test]
#[cfg(all(unix, not(target_os = "scarlet")))]
fn backend_close_reaps_shell_and_last_terminal_drop_cancels() {
    let mut pty = backend::Pty::open(&std::env::temp_dir(), (80, 16)).unwrap();
    pty.write(b"exec /bin/sh -i\r").unwrap();
    pty.write(b"trap '' HUP; sleep 30\r").unwrap();
    thread::sleep(Duration::from_millis(100));
    let pid = pty.pid_for_test();
    drop(pty);
    // Reaped children are no longer waitable by the owning app.
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    let terminal = Terminal::new();
    terminal.open(std::env::temp_dir());
    let cancel = terminal
        .shared
        .session
        .lock()
        .unwrap()
        .as_ref()
        .unwrap()
        .cancel
        .clone();
    let retained = terminal.clone();
    drop(terminal);
    assert!(!cancel.load(Ordering::Acquire));
    drop(retained);
    assert!(cancel.load(Ordering::Acquire));
}
