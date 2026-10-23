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
fn screen_snapshot_keeps_cursor_on_grid_and_off_wide_continuations() {
    for (output, raw_position, display_position) in [
        ("abcd", (0, 4), (0, 3)),
        ("abcdX", (1, 1), (1, 1)),
        ("abcdefgh", (1, 4), (1, 3)),
        ("ab界", (0, 4), (0, 2)),
        ("界\x1b[1;2H", (0, 1), (0, 0)),
        ("界\x1b[1;3H", (0, 2), (0, 2)),
    ] {
        let mut parser = vt100::Parser::new(2, 4, 0);
        parser.process(output.as_bytes());
        assert_eq!(parser.screen().cursor_position(), raw_position);
        let snap = screen::snapshot(parser.screen(), TerminalStatus::Running);
        assert_eq!(
            (snap.cursor.row, snap.cursor.column),
            display_position,
            "{output:?}"
        );
        assert!(snap.cursor.visible);
        // Rendering the cursor must not consume VT100's pending wrap state.
        assert_eq!(parser.screen().cursor_position(), raw_position);
    }
}

#[test]
fn screen_snapshot_preserves_faint_styles_colors_and_resets() {
    let mut parser = vt100::Parser::new(1, 8, 0);
    parser.process(b"\x1b[2;3;4;7;38;2;12;34;56;48;5;196mX\x1b[1mY\x1b[0mZ");
    // Erased cells still carry the active background and rendition.
    parser.process(b"\x1b[2;48;5;244m\x1b[K");
    let snap = screen::snapshot(parser.screen(), TerminalStatus::Running);
    let faint = snap.grid.cell(0, 0).unwrap();
    assert!(faint.faint && faint.italic && faint.underline && faint.inverse);
    assert!(!faint.bold);
    assert_eq!(faint.foreground, scarlet_ui::Color::rgb(12, 34, 56));
    assert_eq!(faint.background, scarlet_ui::Color::rgb(255, 0, 0));
    let bold = snap.grid.cell(1, 0).unwrap();
    assert!(bold.bold && bold.italic && bold.underline && bold.inverse);
    assert!(!bold.faint);
    let reset = snap.grid.cell(2, 0).unwrap();
    let default = screen::blank((1, 1)).cell(0, 0).unwrap();
    assert_eq!(reset, scarlet_ui::TextGridCell { ch: 'Z', ..default });
    let erased = snap.grid.cell(7, 0).unwrap();
    assert_eq!(erased.ch, ' ');
    assert!(erased.faint);
    assert_eq!(erased.background, scarlet_ui::Color::rgb(128, 128, 128));
}

#[test]
fn screen_snapshot_tracks_cursor_only_visibility_modes_and_scrollback() {
    let mut parser = vt100::Parser::new(2, 8, 4);
    parser.process(b"one\r\ntwo\r\nthree");
    let before = screen::snapshot(parser.screen(), TerminalStatus::Running);
    assert!(before.cursor.visible);
    parser.process(b"\x1b[?25l\x1b[?1h\x1b[?2004h");
    let hidden = screen::snapshot(parser.screen(), TerminalStatus::Running);
    assert_eq!(hidden.grid, before.grid);
    assert!(!hidden.cursor.visible);
    assert!(hidden.application_cursor && hidden.bracketed_paste);
    parser.process(b"\x1b[?25h\x1b[?1l\x1b[?2004l");
    let shown = screen::snapshot(parser.screen(), TerminalStatus::Running);
    assert_eq!(shown.grid, before.grid);
    assert!(shown.cursor.visible);
    assert!(!shown.application_cursor && !shown.bracketed_paste);
    parser.screen_mut().set_scrollback(1);
    let history = screen::snapshot(parser.screen(), TerminalStatus::Running);
    assert_eq!(history.grid.cell(0, 0).unwrap().ch, 'o');
    assert!(!history.cursor.visible);
    parser.screen_mut().set_scrollback(0);
    let live = screen::snapshot(parser.screen(), TerminalStatus::Running);
    assert_eq!(live.grid, before.grid);
    assert!(live.cursor.visible);
}

#[test]
fn screen_snapshot_keeps_combining_text_aligned_and_clears_overwritten_wide_cells() {
    let mut parser = vt100::Parser::new(2, 8, 0);
    for byte in "e\u{301}界\u{3099}Z".as_bytes() {
        parser.process(&[*byte]);
    }
    assert_eq!(parser.screen().cell(0, 0).unwrap().contents(), "e\u{301}");
    assert_eq!(parser.screen().cell(0, 1).unwrap().contents(), "界\u{3099}");
    let snap = screen::snapshot(parser.screen(), TerminalStatus::Running);
    // TextGrid supports one scalar per cell. Combining marks must not be
    // placed in neighboring cells or shift subsequent characters.
    for (column, ch) in ['e', '界', '\0', 'Z', ' '].into_iter().enumerate() {
        assert_eq!(snap.grid.cell(column, 0).unwrap().ch, ch);
    }
    assert_eq!((snap.cursor.row, snap.cursor.column), (0, 4));
    parser.process(b"\x1b[1;3HQ");
    let overwritten = screen::snapshot(parser.screen(), TerminalStatus::Running);
    assert_eq!(overwritten.grid.cell(1, 0).unwrap().ch, ' ');
    assert_eq!(overwritten.grid.cell(2, 0).unwrap().ch, 'Q');
    assert_eq!(overwritten.grid.cell(3, 0).unwrap().ch, 'Z');
    parser.process("\r界\rA".as_bytes());
    let replaced = screen::snapshot(parser.screen(), TerminalStatus::Running);
    assert_eq!(replaced.grid.cell(0, 0).unwrap().ch, 'A');
    assert_eq!(replaced.grid.cell(1, 0).unwrap().ch, ' ');
}

#[test]
fn screen_snapshot_stays_bounded_and_recovers_after_malformed_output() {
    let mut parser = vt100::Parser::new(2, 8, 0);
    let output = b"\xff\xfe\xe2\x82\x1b[999999999999999999999;999999999999999999999H\
        \x1b[38;2;999;0;0m?\x1b[31;\x1b[0m\x1b]0;unfinished\
        \x07\x1b[0m\x1b[2J\x1b[HOK";
    for byte in output {
        parser.process(&[*byte]);
        let snap = screen::snapshot(parser.screen(), TerminalStatus::Running);
        assert_eq!((snap.grid.columns(), snap.grid.rows()), (8, 2));
        assert!(snap.cursor.column < 8 && snap.cursor.row < 2);
    }
    let snap = screen::snapshot(parser.screen(), TerminalStatus::Running);
    let mut expected = screen::blank((8, 2));
    for (column, ch) in ['O', 'K'].into_iter().enumerate() {
        let mut cell = expected.cell(column, 0).unwrap();
        cell.ch = ch;
        expected.set_cell(column, 0, cell);
    }
    assert_eq!(snap.grid, expected);
    assert_eq!((snap.cursor.row, snap.cursor.column), (0, 2));
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
