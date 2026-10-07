use super::super::{
    Arc, BTreeMap, CORE_TERMINAL_TYPE, ConsoleProvider, CreateObject, LinuxConsole, Mutex,
    ObjectId, ObjectProvider, ProviderError, ProviderOutcome, TerminalScreen, Value, VecDeque,
};

#[derive(Debug)]
pub(crate) struct HostTerminalProvider {
    host_terminal: ObjectId,
    terminal: Arc<LinuxConsole>,
    screens: Mutex<BTreeMap<ObjectId, TerminalScreen>>,
    canonical_input: Mutex<BTreeMap<(ObjectId, ObjectId), VecDeque<u8>>>,
}

impl HostTerminalProvider {
    pub(crate) fn new(host_terminal: ObjectId, terminal: Arc<LinuxConsole>) -> Self {
        Self {
            host_terminal,
            terminal,
            screens: Mutex::new(BTreeMap::new()),
            canonical_input: Mutex::new(BTreeMap::new()),
        }
    }

    fn poll_canonical(
        &self,
        process: ObjectId,
        object: ObjectId,
        maximum: usize,
        echo: bool,
    ) -> Result<Vec<u8>, ProviderError> {
        let key = (process, object);
        {
            let mut pending = self
                .canonical_input
                .lock()
                .map_err(|_| ProviderError::Unavailable)?;
            if let Some(queue) = pending.get_mut(&key) {
                if !queue.is_empty() {
                    let count = maximum.min(queue.len());
                    return Ok(queue.drain(..count).collect());
                }
            }
        }
        let Some(line) = self.terminal.poll_terminal_line(process, echo)? else {
            return Ok(Vec::new());
        };
        let mut bytes = line.into_bytes();
        bytes.push(b'\n');
        let mut pending = self
            .canonical_input
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        let queue = pending.entry(key).or_default();
        queue.extend(bytes);
        let count = maximum.min(queue.len());
        Ok(queue.drain(..count).collect())
    }

    fn output(
        &self,
        object: ObjectId,
        state: &Value,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        let [Value::Bytes(bytes)] = arguments else {
            return Err(ProviderError::InvalidArguments(
                "Terminal output expects Bytes",
            ));
        };
        if object == self.host_terminal {
            self.terminal
                .render(bytes)
                .map_err(ProviderError::Adapter)?;
        }
        let fields = record(state)?;
        let mut screens = self
            .screens
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        screens
            .entry(object)
            .or_insert_with(|| {
                TerminalScreen::new(
                    positive_dimension(fields, "columns", 80, 512).unwrap_or(80),
                    positive_dimension(fields, "rows", 24, 256).unwrap_or(24),
                )
            })
            .write(bytes);
        Ok(ProviderOutcome::result(Value::Integer(
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        )))
    }

    fn input(
        &self,
        process: ObjectId,
        object: ObjectId,
        state: &Value,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        let [Value::Integer(maximum)] = arguments else {
            return Err(ProviderError::InvalidArguments(
                "Terminal input expects a maximum byte count",
            ));
        };
        let maximum = usize::try_from(*maximum).map_err(|_| {
            ProviderError::InvalidArguments("input byte count must be between 1 and 65536")
        })?;
        if !(1..=65_536).contains(&maximum) {
            return Err(ProviderError::InvalidArguments(
                "input byte count must be between 1 and 65536",
            ));
        }
        if object != self.host_terminal {
            return Ok(ProviderOutcome::result(Value::Bytes(Vec::new())));
        }
        let fields = record(state)?;
        let mode = match fields.get("input_mode") {
            None => "raw",
            Some(Value::Text(mode)) if mode == "raw" => "raw",
            Some(Value::Text(mode)) if mode == "canonical" => "canonical",
            _ => {
                return Err(ProviderError::InvalidArguments(
                    "Terminal input_mode must be raw or canonical",
                ));
            }
        };
        let echo = matches!(fields.get("echo"), Some(Value::Bool(true)));
        let bytes = if mode == "raw" {
            self.terminal.poll_terminal_bytes(process, maximum)?
        } else {
            self.poll_canonical(process, object, maximum, echo)?
        };
        Ok(ProviderOutcome::result(Value::Bytes(bytes)))
    }

    fn snapshot(&self, object: ObjectId, state: &Value) -> Result<ProviderOutcome, ProviderError> {
        let fields = record(state)?;
        let mut screens = self
            .screens
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        let screen = screens.entry(object).or_insert_with(|| {
            TerminalScreen::new(
                positive_dimension(fields, "columns", 80, 512).unwrap_or(80),
                positive_dimension(fields, "rows", 24, 256).unwrap_or(24),
            )
        });
        let dirty_rows = screen.take_dirty_rows();
        let lines = (0..screen.rows())
            .map(|row| Value::Text(screen.row_text(row).unwrap_or_default()))
            .collect();
        let (cursor_row, cursor_column) = screen.cursor();
        Ok(ProviderOutcome::result(Value::Record(BTreeMap::from([
            (
                "columns".to_owned(),
                Value::Integer(i64::try_from(screen.columns()).unwrap_or(i64::MAX)),
            ),
            (
                "rows".to_owned(),
                Value::Integer(i64::try_from(screen.rows()).unwrap_or(i64::MAX)),
            ),
            (
                "cursor".to_owned(),
                Value::Record(BTreeMap::from([
                    (
                        "row".to_owned(),
                        Value::Integer(i64::try_from(cursor_row).unwrap_or(i64::MAX)),
                    ),
                    (
                        "column".to_owned(),
                        Value::Integer(i64::try_from(cursor_column).unwrap_or(i64::MAX)),
                    ),
                ])),
            ),
            ("lines".to_owned(), Value::Array(lines)),
            (
                "dirty_rows".to_owned(),
                Value::Array(
                    dirty_rows
                        .into_iter()
                        .map(|row| Value::Integer(i64::try_from(row).unwrap_or(i64::MAX)))
                        .collect(),
                ),
            ),
            (
                "alternate_screen".to_owned(),
                Value::Bool(screen.is_alternate_screen()),
            ),
            (
                "cursor_visible".to_owned(),
                Value::Bool(screen.cursor_visible()),
            ),
            (
                "bracketed_paste".to_owned(),
                Value::Bool(screen.bracketed_paste()),
            ),
            ("title".to_owned(), Value::Text(screen.title().to_owned())),
        ]))))
    }
}

impl ObjectProvider for HostTerminalProvider {
    fn type_id(&self) -> oms_types::TypeId {
        CORE_TERMINAL_TYPE
    }

    fn user_creatable(&self) -> bool {
        true
    }

    fn create(&self, initial: &Value) -> Result<Value, ProviderError> {
        let mut fields = match initial {
            Value::Null => BTreeMap::new(),
            Value::Map(values) | Value::Record(values) => values.clone(),
            _ => {
                return Err(ProviderError::InvalidArguments(
                    "Terminal state must be a Record or null",
                ));
            }
        };
        if !fields.contains_key("columns") {
            fields.insert("columns".to_owned(), Value::Integer(80));
        }
        if !fields.contains_key("rows") {
            fields.insert("rows".to_owned(), Value::Integer(24));
        }
        if !fields.contains_key("input_mode") {
            fields.insert("input_mode".to_owned(), Value::Text("raw".to_owned()));
        }
        if !fields.contains_key("echo") {
            fields.insert("echo".to_owned(), Value::Bool(false));
        }
        validate_configuration(&fields)?;
        Ok(Value::Record(fields))
    }

    fn invoke(
        &self,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        let mut fields = record(state)?.clone();
        match (capability, arguments) {
            ("create", [] | [Value::Record(_) | Value::Map(_)]) => {
                let initial = arguments.first().cloned().unwrap_or(Value::Null);
                let child_state = self.create(&initial)?;
                let id = ObjectId::new();
                let request = CreateObject::new(CORE_TERMINAL_TYPE, child_state.encode()?)
                    .with_id(id)
                    .with_parent(object);
                Ok(ProviderOutcome::result(Value::Text(id.to_string())).with_created(request))
            }
            ("configure", [Value::Record(configuration) | Value::Map(configuration)]) => {
                for (key, value) in configuration {
                    match key.as_str() {
                        "input_mode" | "echo" => {
                            fields.insert(key.clone(), value.clone());
                        }
                        _ => {
                            return Err(ProviderError::InvalidArguments(
                                "Terminal configure accepts input_mode and echo",
                            ));
                        }
                    }
                }
                validate_configuration(&fields)?;
                Ok(ProviderOutcome::result(Value::Null).with_state(Value::Record(fields)))
            }
            ("resize", [Value::Integer(columns), Value::Integer(rows)]) => {
                let columns = usize::try_from(*columns).map_err(|_| {
                    ProviderError::InvalidArguments("Terminal width must be between 1 and 512")
                })?;
                let rows = usize::try_from(*rows).map_err(|_| {
                    ProviderError::InvalidArguments("Terminal height must be between 1 and 256")
                })?;
                if !(1..=512).contains(&columns) || !(1..=256).contains(&rows) {
                    return Err(ProviderError::InvalidArguments(
                        "Terminal size must be between 1x1 and 512x256",
                    ));
                }
                fields.insert(
                    "columns".to_owned(),
                    Value::Integer(i64::try_from(columns).expect("bounded dimension")),
                );
                fields.insert(
                    "rows".to_owned(),
                    Value::Integer(i64::try_from(rows).expect("bounded dimension")),
                );
                let mut screens = self
                    .screens
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?;
                screens
                    .entry(object)
                    .or_insert_with(|| TerminalScreen::new(columns, rows))
                    .resize(columns, rows);
                Ok(ProviderOutcome::result(Value::Null).with_state(Value::Record(fields)))
            }
            _ => Err(ProviderError::InvalidArguments(
                "Terminal expects create(), configure(record), or resize(columns, rows)",
            )),
        }
    }

    fn ephemeral_capabilities(&self) -> std::collections::BTreeSet<String> {
        ["input", "output", "snapshot"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }

    fn invoke_ephemeral_for_process(
        &self,
        process: ObjectId,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        match capability {
            "output" => self.output(object, state, arguments),
            "input" => self.input(process, object, state, arguments),
            "snapshot" if arguments.is_empty() => self.snapshot(object, state),
            _ => Err(ProviderError::InvalidArguments(
                "Terminal input(n), output(bytes), or snapshot() expected",
            )),
        }
    }

    fn process_ended(&self, process: ObjectId) {
        self.terminal.release_process_input(process);
        if let Ok(mut pending) = self.canonical_input.lock() {
            pending.retain(|(owner, _), _| *owner != process);
        }
    }

    fn capabilities(&self) -> std::collections::BTreeSet<String> {
        ["create", "configure", "resize"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}

fn record(value: &Value) -> Result<&BTreeMap<String, Value>, ProviderError> {
    match value {
        Value::Map(fields) | Value::Record(fields) => Ok(fields),
        _ => Err(ProviderError::InvalidArguments(
            "Terminal state must be a Record",
        )),
    }
}

fn positive_dimension(
    fields: &BTreeMap<String, Value>,
    name: &str,
    default: usize,
    maximum: usize,
) -> Result<usize, ProviderError> {
    match fields.get(name) {
        None => Ok(default),
        Some(Value::Integer(value)) => {
            let dimension = usize::try_from(*value).map_err(|_| {
                ProviderError::InvalidArguments("Terminal dimensions must be positive")
            })?;
            if !(1..=maximum).contains(&dimension) {
                return Err(ProviderError::InvalidArguments(
                    "Terminal dimensions exceed the supported range",
                ));
            }
            Ok(dimension)
        }
        _ => Err(ProviderError::InvalidArguments(
            "Terminal dimensions must be Integer values",
        )),
    }
}

fn validate_configuration(fields: &BTreeMap<String, Value>) -> Result<(), ProviderError> {
    positive_dimension(fields, "columns", 80, 512)?;
    positive_dimension(fields, "rows", 24, 256)?;
    match fields.get("input_mode") {
        None => {}
        Some(Value::Text(mode)) if mode == "raw" || mode == "canonical" => {}
        _ => {
            return Err(ProviderError::InvalidArguments(
                "Terminal input_mode must be raw or canonical",
            ));
        }
    }
    if matches!(fields.get("echo"), Some(Value::Bool(_)) | None) {
        Ok(())
    } else {
        Err(ProviderError::InvalidArguments(
            "Terminal echo must be Boolean",
        ))
    }
}
