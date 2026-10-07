use super::super::{
    Arc, AtomicBool, BTreeMap, BTreeSet, Command, ControlFlags, InputFlags, IsTerminal, LocalFlags,
    Mutex, ObjectId, Ordering, ProviderError, SetArg, SpecialCharacterIndices, Stdio,
    TerminalProvider, Termios, Value, VecDeque, Write, error_text, mpsc, tcgetattr, tcsetattr,
};
use super::input::{dispatch_event, input_reader};
use super::key_parser::KeyEvent;

#[derive(Debug)]
pub(crate) struct LinuxTerminal {
    pub(crate) input: Arc<Mutex<LinuxInputState>>,
    pub(crate) running: Arc<AtomicBool>,
    pub(crate) start_reader: mpsc::Sender<()>,
    pub(crate) reader: Mutex<Option<std::thread::JoinHandle<()>>>,
    pub(crate) original_termios: Mutex<Option<Termios>>,
    pub(crate) terminal: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InputMode {
    Line,
    Secret,
    Keyboard,
    TerminalRaw,
    TerminalCanonical { echo: bool },
}

#[derive(Debug)]
pub(crate) struct LinuxInputState {
    pub(crate) owner: Option<(ObjectId, InputMode)>,
    pub(crate) line: String,
    pub(crate) completed_lines: BTreeMap<ObjectId, Result<String, String>>,
    pub(crate) events: BTreeMap<ObjectId, VecDeque<Value>>,
    pub(crate) overflowed: BTreeSet<ObjectId>,
    pub(crate) raw_bytes: BTreeMap<ObjectId, VecDeque<u8>>,
    pub(crate) raw_overflowed: BTreeSet<ObjectId>,
    pub(crate) interrupt_watchers: BTreeSet<ObjectId>,
    pub(crate) interrupted: BTreeSet<ObjectId>,
    pub(crate) unclaimed: VecDeque<KeyEvent>,
    pub(crate) eof: bool,
    pub(crate) reader_error: Option<String>,
}

impl LinuxTerminal {
    pub(crate) fn new() -> Result<Arc<Self>, String> {
        let stdin = std::io::stdin();
        let terminal = stdin.is_terminal();
        let original_termios = if terminal {
            let original = tcgetattr(&stdin).map_err(error_text)?;
            let mut raw = original.clone();
            raw.local_flags.remove(
                LocalFlags::ICANON | LocalFlags::ECHO | LocalFlags::IEXTEN | LocalFlags::ISIG,
            );
            raw.input_flags.remove(
                InputFlags::ICRNL
                    | InputFlags::INLCR
                    | InputFlags::IGNCR
                    | InputFlags::IXON
                    | InputFlags::BRKINT
                    | InputFlags::ISTRIP
                    | InputFlags::INPCK,
            );
            raw.control_flags.remove(ControlFlags::CSIZE);
            raw.control_flags.insert(ControlFlags::CS8);
            raw.control_chars[SpecialCharacterIndices::VMIN as usize] = 0;
            raw.control_chars[SpecialCharacterIndices::VTIME as usize] = 1;
            tcsetattr(&stdin, SetArg::TCSANOW, &raw).map_err(error_text)?;
            Some(original)
        } else {
            None
        };
        let input = Arc::new(Mutex::new(LinuxInputState {
            owner: None,
            line: String::new(),
            completed_lines: BTreeMap::new(),
            events: BTreeMap::new(),
            overflowed: BTreeSet::new(),
            raw_bytes: BTreeMap::new(),
            raw_overflowed: BTreeSet::new(),
            interrupt_watchers: BTreeSet::new(),
            interrupted: BTreeSet::new(),
            unclaimed: VecDeque::new(),
            eof: false,
            reader_error: None,
        }));
        let running = Arc::new(AtomicBool::new(true));
        let (start_reader, start_receiver) = mpsc::channel();
        let reader_input = Arc::clone(&input);
        let reader_running = Arc::clone(&running);
        let reader = match std::thread::Builder::new()
            .name("ousject-terminal-input".to_owned())
            .spawn(move || {
                input_reader(&reader_input, &reader_running, terminal, &start_receiver);
            }) {
            Ok(reader) => reader,
            Err(error) => {
                if let Some(original) = &original_termios {
                    let _ = tcsetattr(&stdin, SetArg::TCSANOW, original);
                }
                return Err(error.to_string());
            }
        };
        Ok(Arc::new(Self {
            input,
            running,
            start_reader,
            reader: Mutex::new(Some(reader)),
            original_termios: Mutex::new(original_termios),
            terminal,
        }))
    }

    pub(crate) fn poll_line(
        &self,
        process: ObjectId,
        mode: InputMode,
    ) -> Result<Option<String>, String> {
        let mut input = self.input.lock().map_err(error_text)?;
        if let Some(result) = input.completed_lines.remove(&process) {
            return result.map(Some);
        }
        if let Some(error) = &input.reader_error {
            return Err(error.clone());
        }
        if input.eof {
            return Err("terminal input reached EOF".to_owned());
        }
        let (start_reader, pending) = match input.owner {
            None => {
                input.owner = Some((process, mode));
                input.line.clear();
                (true, std::mem::take(&mut input.unclaimed))
            }
            Some((owner, owner_mode)) if owner == process && owner_mode == mode => {
                (false, VecDeque::new())
            }
            Some((owner, _)) if owner == process => {
                input.owner = Some((process, mode));
                input.line.clear();
                input.completed_lines.remove(&process);
                input.events.remove(&process);
                (false, VecDeque::new())
            }
            Some(_) => return Ok(None),
        };
        drop(input);
        for event in pending {
            dispatch_event(&self.input, event, self.terminal);
        }
        if let Some(result) = self
            .input
            .lock()
            .map_err(error_text)?
            .completed_lines
            .remove(&process)
        {
            return result.map(Some);
        }
        if start_reader {
            self.start_reader.send(()).map_err(error_text)?;
        }
        Ok(None)
    }

    pub(crate) fn begin_interrupt_watch(&self, process: ObjectId) -> Result<(), String> {
        if !self.terminal {
            return Ok(());
        }
        let mut input = self.input.lock().map_err(error_text)?;
        let start_reader = input.interrupt_watchers.is_empty() && input.owner.is_none();
        input.interrupt_watchers.insert(process);
        drop(input);
        if start_reader {
            self.start_reader.send(()).map_err(error_text)?;
        }
        Ok(())
    }

    pub(crate) fn take_interrupt(&self, process: ObjectId) -> Result<bool, String> {
        let mut input = self.input.lock().map_err(error_text)?;
        Ok(input.interrupted.remove(&process))
    }

    pub(crate) fn end_interrupt_watch(&self, process: ObjectId) {
        if let Ok(mut input) = self.input.lock() {
            input.interrupt_watchers.remove(&process);
            input.interrupted.remove(&process);
        }
    }

    pub(crate) fn capture_keyboard(&self, process: ObjectId) -> Result<(), ProviderError> {
        let mut input = self.input.lock().map_err(|_| ProviderError::Unavailable)?;
        let start_reader = match input.owner {
            None => {
                input.owner = Some((process, InputMode::Keyboard));
                input.events.entry(process).or_default();
                input.overflowed.remove(&process);
                true
            }
            Some((owner, InputMode::Keyboard)) if owner == process => false,
            Some(_) => return Err(ProviderError::Pending),
        };
        drop(input);
        if start_reader {
            self.start_reader
                .send(())
                .map_err(|_| ProviderError::Unavailable)?;
        }
        Ok(())
    }

    pub(crate) fn poll_terminal_bytes(
        &self,
        process: ObjectId,
        maximum: usize,
    ) -> Result<Vec<u8>, ProviderError> {
        let mut input = self.input.lock().map_err(|_| ProviderError::Unavailable)?;
        let start_reader = match input.owner {
            None => {
                input.owner = Some((process, InputMode::TerminalRaw));
                input.raw_bytes.entry(process).or_default();
                input.raw_overflowed.remove(&process);
                true
            }
            Some((owner, InputMode::TerminalRaw)) if owner == process => false,
            Some((owner, _)) if owner == process => {
                input.owner = Some((process, InputMode::TerminalRaw));
                input.line.clear();
                input.completed_lines.remove(&process);
                input.events.remove(&process);
                input.raw_bytes.entry(process).or_default();
                input.raw_overflowed.remove(&process);
                false
            }
            // This is a non-blocking byte-stream poll. Another owner's lease
            // means there are no bytes available to this Process right now.
            Some(_) => return Ok(Vec::new()),
        };
        if input.raw_overflowed.remove(&process) {
            return Err(ProviderError::Adapter(
                "terminal input queue overflowed; release and capture again".to_owned(),
            ));
        }
        let queue = input.raw_bytes.entry(process).or_default();
        let count = maximum.min(queue.len());
        let bytes = queue.drain(..count).collect();
        drop(input);
        if start_reader {
            self.start_reader
                .send(())
                .map_err(|_| ProviderError::Unavailable)?;
        }
        Ok(bytes)
    }

    pub(crate) fn poll_terminal_line(
        &self,
        process: ObjectId,
        echo: bool,
    ) -> Result<Option<String>, ProviderError> {
        self.poll_line(process, InputMode::TerminalCanonical { echo })
            .map_err(ProviderError::Adapter)
    }

    pub(crate) fn release_keyboard(&self, process: ObjectId) -> Result<(), ProviderError> {
        let mut input = self.input.lock().map_err(|_| ProviderError::Unavailable)?;
        match input.owner {
            None => Ok(()),
            Some((owner, InputMode::Keyboard)) if owner == process => {
                input.owner = None;
                input.events.remove(&process);
                input.overflowed.remove(&process);
                Ok(())
            }
            Some((owner, _)) if owner != process => Err(ProviderError::Adapter(
                "terminal input belongs to another Process".to_owned(),
            )),
            Some(_) => Err(ProviderError::Adapter(
                "this Process does not own keyboard capture".to_owned(),
            )),
        }
    }

    pub(crate) fn take_key_event(&self, process: ObjectId) -> Result<Option<Value>, ProviderError> {
        let mut input = self.input.lock().map_err(|_| ProviderError::Unavailable)?;
        if input.owner != Some((process, InputMode::Keyboard)) {
            return Err(ProviderError::Adapter(
                "call keyboard.capture() before reading key events".to_owned(),
            ));
        }
        if input.overflowed.remove(&process) {
            return Err(ProviderError::Adapter(
                "keyboard event queue overflowed; release and capture again".to_owned(),
            ));
        }
        Ok(input.events.entry(process).or_default().pop_front())
    }

    pub(crate) fn take_key_events(
        &self,
        process: ObjectId,
        maximum: usize,
    ) -> Result<Vec<Value>, ProviderError> {
        let mut input = self.input.lock().map_err(|_| ProviderError::Unavailable)?;
        if input.owner != Some((process, InputMode::Keyboard)) {
            return Err(ProviderError::Adapter(
                "call keyboard.capture() before reading key events".to_owned(),
            ));
        }
        if input.overflowed.remove(&process) {
            return Err(ProviderError::Adapter(
                "keyboard event queue overflowed; release and capture again".to_owned(),
            ));
        }
        let events = input.events.entry(process).or_default();
        let count = maximum.min(events.len());
        Ok(events.drain(..count).collect())
    }

    pub(crate) fn release_process_input(&self, process: ObjectId) {
        if let Ok(mut input) = self.input.lock() {
            if input.owner.is_some_and(|(owner, _)| owner == process) {
                input.owner = None;
                input.line.clear();
            }
            input.completed_lines.remove(&process);
            input.events.remove(&process);
            input.overflowed.remove(&process);
            input.raw_bytes.remove(&process);
            input.raw_overflowed.remove(&process);
            input.interrupt_watchers.remove(&process);
            input.interrupted.remove(&process);
        }
    }
}

impl TerminalProvider for LinuxTerminal {
    fn print(&self, text: &str) -> Result<(), String> {
        let mut output = std::io::stdout().lock();
        output.write_all(text.as_bytes()).map_err(error_text)?;
        output.flush().map_err(error_text)
    }

    fn println(&self, text: &str) -> Result<(), String> {
        writeln!(std::io::stdout().lock(), "{text}").map_err(error_text)
    }

    fn render(&self, frame: &[u8]) -> Result<(), String> {
        let mut output = std::io::stdout().lock();
        output.write_all(frame).map_err(error_text)?;
        output.flush().map_err(error_text)
    }

    fn size(&self) -> Result<(u16, u16), String> {
        let output = Command::new("stty")
            .arg("size")
            .stdin(Stdio::inherit())
            .stderr(Stdio::null())
            .output()
            .map_err(error_text)?;
        if !output.status.success() {
            return Err("could not read terminal size".to_owned());
        }
        let values = String::from_utf8(output.stdout).map_err(error_text)?;
        let mut dimensions = values.split_whitespace();
        let rows = dimensions
            .next()
            .ok_or_else(|| "terminal did not report its row count".to_owned())?
            .parse::<u16>()
            .map_err(error_text)?;
        let columns = dimensions
            .next()
            .ok_or_else(|| "terminal did not report its column count".to_owned())?
            .parse::<u16>()
            .map_err(error_text)?;
        Ok((columns, rows))
    }

    fn is_interactive(&self) -> bool {
        std::io::stdin().is_terminal() && std::io::stdout().is_terminal()
    }

    fn begin_interrupt_watch(&self, process: ObjectId) -> Result<(), String> {
        LinuxTerminal::begin_interrupt_watch(self, process)
    }

    fn take_interrupt(&self, process: ObjectId) -> Result<bool, String> {
        LinuxTerminal::take_interrupt(self, process)
    }

    fn end_interrupt_watch(&self, process: ObjectId) {
        LinuxTerminal::end_interrupt_watch(self, process);
    }

    fn try_read_line(&self) -> Result<Option<String>, String> {
        self.poll_line(ObjectId::new(), InputMode::Line)
    }

    fn try_read_line_for(&self, process: ObjectId) -> Result<Option<String>, String> {
        self.poll_line(process, InputMode::Line)
    }

    fn try_read_secret(&self) -> Result<Option<String>, String> {
        self.poll_line(ObjectId::new(), InputMode::Secret)
    }

    fn try_read_secret_for(&self, process: ObjectId) -> Result<Option<String>, String> {
        self.poll_line(process, InputMode::Secret)
    }

    fn release_process(&self, process: ObjectId) {
        self.release_process_input(process);
    }
}

impl Drop for LinuxTerminal {
    fn drop(&mut self) {
        self.running.store(false, Ordering::Release);
        let _ = self.start_reader.send(());
        if self.terminal {
            if let Ok(mut reader) = self.reader.lock() {
                if let Some(reader) = reader.take() {
                    let _ = reader.join();
                }
            }
        }
        if let Ok(original) = self.original_termios.lock() {
            if let Some(original) = original.as_ref() {
                let _ = tcsetattr(std::io::stdin(), SetArg::TCSANOW, original);
            }
        }
    }
}
