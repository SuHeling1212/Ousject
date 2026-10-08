#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunReport {
    pub process: ObjectId,
    pub steps: u64,
    pub status: ProcessStatus,
    pub output: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScheduleReport {
    pub total_steps: u64,
    pub processes: Vec<RunReport>,
}

/// Background kernel worker that retires terminal Process trees after the
/// seven-day result-retention window. Tombstone metadata remains in OMS.
#[derive(Debug)]
pub struct ProcessReaper {
    messages: mpsc::Sender<ProcessReaperMessage>,
    worker: Option<JoinHandle<()>>,
}

#[derive(Debug)]
enum ProcessReaperMessage {
    Wake(ObjectId),
    Shutdown,
}

impl ProcessReaper {
    /// Starts the automatic finished-Process cleanup worker.
    ///
    /// # Errors
    ///
    /// Returns an error if the initial cleanup or worker creation fails.
    pub fn start(vm: &VirtualMachine, interval: Duration) -> Result<Self, VmError> {
        let manager = Arc::clone(&vm.manager);
        let (messages, receiver) = mpsc::channel();
        *vm.process_reaper
            .lock()
            .map_err(|_| VmError::Provider("Process reaper notifier unavailable".to_owned()))? =
            Some(messages.clone());
        let startup = reap_expired_processes(&manager, SystemTime::now())
            .and_then(|()| finished_process_deadlines(&manager));
        let deadlines = match startup {
            Ok(deadlines) => deadlines,
            Err(error) => {
                if let Ok(mut notifier) = vm.process_reaper.lock() {
                    *notifier = None;
                }
                return Err(error);
            }
        };
        let worker = std::thread::Builder::new()
            .name("ousject-process-reaper".to_owned())
            .spawn(move || process_reaper_loop(&manager, deadlines, &receiver, interval))
            .map_err(|error| VmError::Provider(error.to_string()));
        let worker = match worker {
            Ok(worker) => worker,
            Err(error) => {
                if let Ok(mut notifier) = vm.process_reaper.lock() {
                    *notifier = None;
                }
                return Err(error);
            }
        };
        Ok(Self {
            messages,
            worker: Some(worker),
        })
    }
}

impl Drop for ProcessReaper {
    fn drop(&mut self) {
        let _ = self.messages.send(ProcessReaperMessage::Shutdown);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl From<ProviderError> for VmError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error.to_string())
    }
}

pub trait TerminalProvider: fmt::Debug + Send + Sync {
    /// Sends output without adding a trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific message when the device is unavailable.
    fn print(&self, text: &str) -> Result<(), String> {
        self.println(text)
    }

    /// Sends output whose durable Effect intent is already committed.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific message when the device is unavailable.
    fn println(&self, text: &str) -> Result<(), String>;

    /// Renders transient terminal bytes. This output is intentionally not a
    /// durable Effect; callers redraw it from persistent application state.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific error when the terminal cannot render it.
    fn render(&self, frame: &[u8]) -> Result<(), String> {
        let text = std::str::from_utf8(frame).map_err(|error| error.to_string())?;
        self.print(text)
    }

    /// Reads terminal dimensions.
    ///
    /// # Errors
    ///
    /// Returns an error when dimensions cannot be determined.
    fn size(&self) -> Result<(u16, u16), String> {
        Err("terminal size is unavailable".to_owned())
    }

    fn is_interactive(&self) -> bool {
        false
    }

    /// Begins a temporary raw-input watch while the VM waits for a terminal
    /// submission. Providers without interrupt support may ignore the request.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific error when the watch cannot be started.
    fn begin_interrupt_watch(&self, _process: ObjectId) -> Result<(), String> {
        Ok(())
    }

    /// Returns and clears a pending Ctrl+C event for a watched Process.
    ///
    /// # Errors
    ///
    /// Returns an adapter-specific error when interrupt state cannot be read.
    fn take_interrupt(&self, _process: ObjectId) -> Result<bool, String> {
        Ok(false)
    }

    /// Ends a temporary raw-input watch.
    fn end_interrupt_watch(&self, _process: ObjectId) {}

    /// Reads one UTF-8 line and removes its trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific message when terminal input is unavailable.
    fn try_read_line(&self) -> Result<Option<String>, String> {
        Ok(None)
    }

    /// Reads a line for one Process through a shared terminal adapter.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when input is unavailable.
    fn try_read_line_for(&self, _process: ObjectId) -> Result<Option<String>, String> {
        self.try_read_line()
    }

    /// Reads a secret line without echo when the hardware adapter supports it.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific message when secure input is unavailable.
    fn try_read_secret(&self) -> Result<Option<String>, String> {
        self.try_read_line()
    }

    /// Reads a secret line for one Process without exposing it to others.
    ///
    /// # Errors
    ///
    /// Returns an adapter error when secret input is unavailable.
    fn try_read_secret_for(&self, _process: ObjectId) -> Result<Option<String>, String> {
        self.try_read_secret()
    }

    fn release_process(&self, _process: ObjectId) {}
}
