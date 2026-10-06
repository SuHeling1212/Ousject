use super::super::{AtomicBool, Duration, Mutex, Ordering, Read, Write, mpsc};
use super::console::{InputMode, LinuxInputState};
use super::key_parser::{InputParser, KeyEvent, MAX_KEYBOARD_EVENTS};

pub(super) fn input_reader(
    input: &Mutex<LinuxInputState>,
    running: &AtomicBool,
    terminal: bool,
    start: &mpsc::Receiver<()>,
) {
    let stdin = std::io::stdin();
    let mut stdin = stdin.lock();
    let mut parser = InputParser::default();
    while running.load(Ordering::Acquire) {
        match start.recv_timeout(Duration::from_millis(100)) {
            Ok(()) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        }
        if !running.load(Ordering::Acquire) {
            break;
        }
        let mut byte = [0_u8; 1];
        loop {
            if !running.load(Ordering::Acquire) {
                break;
            }
            let should_read = input
                .lock()
                .is_ok_and(|state| state.owner.is_some() || !state.interrupt_watchers.is_empty());
            if !should_read {
                parser.clear_pending();
                break;
            }
            match stdin.read(&mut byte) {
                Ok(0) if terminal => {
                    if let Some(event) = parser.flush_escape_timeout(std::time::Instant::now()) {
                        dispatch_event(input, event, terminal);
                    }
                    std::thread::yield_now();
                }
                Ok(0) => {
                    if let Ok(mut state) = input.lock() {
                        state.eof = true;
                    }
                    break;
                }
                Ok(1) => {
                    for event in parser.push(byte[0]) {
                        dispatch_event(input, event, terminal);
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::Interrupted => {}
                Err(error) => {
                    if let Ok(mut state) = input.lock() {
                        state.reader_error = Some(error.to_string());
                    }
                    break;
                }
                Ok(_) => unreachable!("one-byte input buffer has a maximum length of one"),
            }
        }
    }
}

pub(crate) fn dispatch_event(input: &Mutex<LinuxInputState>, event: KeyEvent, terminal: bool) {
    let Ok(mut state) = input.lock() else {
        return;
    };
    let interrupt = event.control && event.key == "c";
    if interrupt {
        let watchers: Vec<_> = state.interrupt_watchers.iter().copied().collect();
        state.interrupted.extend(watchers);
    }
    let Some((process, mode)) = state.owner else {
        if !interrupt && !state.interrupt_watchers.is_empty() {
            if state.unclaimed.len() >= MAX_KEYBOARD_EVENTS {
                return;
            }
            state.unclaimed.push_back(event);
        }
        return;
    };
    if mode == InputMode::Keyboard {
        let queue = state.events.entry(process).or_default();
        if queue.len() >= MAX_KEYBOARD_EVENTS {
            state.overflowed.insert(process);
        } else {
            queue.push_back(event.as_value());
        }
        return;
    }
    match event.key.as_str() {
        "enter" => {
            let line = std::mem::take(&mut state.line);
            state.completed_lines.insert(process, Ok(line));
            state.owner = None;
            if terminal {
                let _ = writeln!(std::io::stdout().lock());
            }
        }
        "backspace" => {
            if state.line.pop().is_some() && mode == InputMode::Line && terminal {
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(b"\x08 \x08");
                let _ = stdout.flush();
            }
        }
        _ if event.control && event.key == "c" => {
            let result = if mode == InputMode::Secret {
                Err("console input interrupted".to_owned())
            } else {
                Ok("\r".to_owned())
            };
            state.completed_lines.insert(process, result);
            state.line.clear();
            state.owner = None;
            if mode == InputMode::Line && terminal {
                let _ = writeln!(std::io::stdout().lock());
            }
        }
        _ => {
            if let Some(text) = event.text {
                state.line.push_str(&text);
                if mode == InputMode::Line && terminal {
                    let mut stdout = std::io::stdout().lock();
                    let _ = stdout.write_all(text.as_bytes());
                    let _ = stdout.flush();
                }
            }
        }
    }
}
