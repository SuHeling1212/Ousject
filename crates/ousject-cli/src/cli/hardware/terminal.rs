use super::super::{
    AccessContext, Arc, BTreeMap, BTreeSet, CORE_TERMINAL_TYPE, ConsoleProvider, CreateObject,
    EffectRecoveryPolicy, InMemoryObjectManager, LinuxConsole, Mutex, ObjectId, ObjectProvider,
    ObjectQuery, ProviderError, ProviderOutcome, SYSTEM_SUBJECT, TerminalScreen, Value, VecDeque,
};
use ousject_provider::{
    MouseTracking, TerminalColor, TerminalModes, TerminalRenderView, TerminalStyle,
};

#[derive(Debug, Default)]
struct RenderModes {
    initialized: bool,
    modes: TerminalModes,
    title: String,
}

#[derive(Debug)]
struct TerminalRoutes {
    active_terminal: ObjectId,
    active_process: Option<ObjectId>,
    parents: BTreeMap<ObjectId, ObjectId>,
    foregrounds: BTreeMap<ObjectId, ObjectId>,
    process_terminals: BTreeMap<ObjectId, ObjectId>,
}

#[derive(Debug)]
pub(crate) struct HostTerminalProvider {
    host_terminal: ObjectId,
    terminal: Arc<LinuxConsole>,
    screens: Mutex<BTreeMap<ObjectId, TerminalScreen>>,
    canonical_input: Mutex<BTreeMap<(ObjectId, ObjectId), VecDeque<u8>>>,
    events: Mutex<BTreeMap<ObjectId, VecDeque<Value>>>,
    render_modes: Mutex<RenderModes>,
    routes: Mutex<TerminalRoutes>,
    manager: Option<Arc<InMemoryObjectManager>>,
}

impl HostTerminalProvider {
    pub(crate) fn new(host_terminal: ObjectId, terminal: Arc<LinuxConsole>) -> Self {
        Self {
            host_terminal,
            terminal,
            screens: Mutex::new(BTreeMap::new()),
            canonical_input: Mutex::new(BTreeMap::new()),
            events: Mutex::new(BTreeMap::new()),
            render_modes: Mutex::new(RenderModes::default()),
            routes: Mutex::new(TerminalRoutes {
                active_terminal: host_terminal,
                active_process: None,
                parents: BTreeMap::new(),
                foregrounds: BTreeMap::new(),
                process_terminals: BTreeMap::new(),
            }),
            manager: None,
        }
    }

    pub(crate) fn with_manager(
        host_terminal: ObjectId,
        terminal: Arc<LinuxConsole>,
        manager: Arc<InMemoryObjectManager>,
    ) -> Self {
        let mut provider = Self::new(host_terminal, terminal);
        provider.manager = Some(manager);
        provider.recover_routes();
        provider
    }

    fn recover_routes(&mut self) {
        let Some(manager) = &self.manager else {
            return;
        };
        let Ok(headers) = manager.query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new().with_type(CORE_TERMINAL_TYPE),
        ) else {
            return;
        };
        let mut routes = TerminalRoutes {
            active_terminal: self.host_terminal,
            active_process: None,
            parents: BTreeMap::new(),
            foregrounds: BTreeMap::new(),
            process_terminals: BTreeMap::new(),
        };
        for header in headers {
            let Ok(Value::Record(fields)) =
                manager.value(AccessContext::new(SYSTEM_SUBJECT), header.id)
            else {
                continue;
            };
            if let Some(parent) = header.parent_id {
                routes.parents.insert(header.id, parent);
            }
            let Some(Value::Text(process)) = fields.get("foreground_process") else {
                continue;
            };
            let Ok(process) = process.parse::<ObjectId>() else {
                continue;
            };
            routes.foregrounds.insert(header.id, process);
            routes.process_terminals.insert(process, header.id);
        }
        let deepest = routes
            .foregrounds
            .keys()
            .copied()
            .max_by_key(|terminal| terminal_depth(&routes.parents, *terminal));
        if let Some(terminal) = deepest {
            routes.active_terminal = terminal;
            routes.active_process = routes.foregrounds.get(&terminal).copied();
        }
        if let Ok(mut current) = self.routes.lock() {
            *current = routes;
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
        screen.write(bytes);
        if self.is_active_terminal(object)? {
            let mut view = screen.render_view();
            let mut modes = self
                .render_modes
                .lock()
                .map_err(|_| ProviderError::Unavailable)?;
            let frame = render_terminal_frame(&mut view, &mut modes);
            drop(modes);
            drop(screens);
            self.terminal
                .render(&frame)
                .map_err(ProviderError::Adapter)?;
        }
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
        if !self.is_active_terminal(object)? {
            return Ok(ProviderOutcome::result(Value::Bytes(Vec::new())));
        }
        let fields = record(state)?;
        if let Some(foreground) = fields.get("foreground_process") {
            match foreground {
                Value::Text(id) if id.parse::<ObjectId>().ok() == Some(process) => {}
                Value::Null => {}
                _ => return Ok(ProviderOutcome::result(Value::Bytes(Vec::new()))),
            }
        } else if object != self.host_terminal {
            return Ok(ProviderOutcome::result(Value::Bytes(Vec::new())));
        }
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

    fn is_active_terminal(&self, object: ObjectId) -> Result<bool, ProviderError> {
        Ok(self
            .routes
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .active_terminal
            == object)
    }

    #[cfg(test)]
    pub(crate) fn active_terminal_id(&self) -> Result<ObjectId, ProviderError> {
        Ok(self
            .routes
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .active_terminal)
    }

    fn activate(
        &self,
        process: ObjectId,
        object: ObjectId,
        fields: &mut BTreeMap<String, Value>,
    ) -> Result<(), ProviderError> {
        let mut routes = self.routes.lock().map_err(|_| ProviderError::Unavailable)?;
        if let Some(previous) = routes
            .active_process
            .filter(|previous| *previous != process)
        {
            let previous_terminal = routes.active_terminal;
            routes.foregrounds.insert(previous_terminal, previous);
            self.terminal.release_process_input(previous);
        }
        routes.active_terminal = object;
        routes.active_process = Some(process);
        routes.foregrounds.insert(object, process);
        routes.process_terminals.insert(process, object);
        fields.insert(
            "foreground_process".to_owned(),
            Value::Text(process.to_string()),
        );
        drop(routes);
        let mut screens = self
            .screens
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        if let Some(screen) = screens.get_mut(&object) {
            screen.mark_all_dirty();
            let mut view = screen.render_view();
            let mut modes = self
                .render_modes
                .lock()
                .map_err(|_| ProviderError::Unavailable)?;
            let frame = render_terminal_frame(&mut view, &mut modes);
            drop(modes);
            drop(screens);
            self.terminal
                .render(&frame)
                .map_err(ProviderError::Adapter)?;
        }
        Ok(())
    }

    fn wait_event(
        &self,
        process: ObjectId,
        object: ObjectId,
        state: &Value,
    ) -> Result<ProviderOutcome, ProviderError> {
        if !self.is_active_terminal(object)? {
            return Err(ProviderError::Pending);
        }
        let mut fields = record(state)?.clone();
        match fields.get("foreground_process") {
            Some(Value::Text(id)) if id.parse::<ObjectId>().ok() == Some(process) => {}
            Some(Value::Null) if object == self.host_terminal => {}
            _ => return Err(ProviderError::Pending),
        }

        let mut resized = false;
        if self.terminal.is_interactive() {
            let (columns, rows) = self.terminal.size().map_err(ProviderError::Adapter)?;
            let columns = usize::from(columns).clamp(1, 512);
            let rows = usize::from(rows).clamp(1, 256);
            let old_columns = positive_dimension(&fields, "columns", 80, 512)?;
            let old_rows = positive_dimension(&fields, "rows", 24, 256)?;
            if (columns, rows) != (old_columns, old_rows) {
                fields.insert(
                    "columns".to_owned(),
                    Value::Integer(i64::try_from(columns).expect("bounded width")),
                );
                fields.insert(
                    "rows".to_owned(),
                    Value::Integer(i64::try_from(rows).expect("bounded height")),
                );
                self.screens
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .entry(object)
                    .or_insert_with(|| TerminalScreen::new(columns, rows))
                    .resize(columns, rows);
                self.push_resize_event(object, columns, rows)?;
                resized = true;
            }
        }

        let event = self
            .events
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get_mut(&object)
            .and_then(VecDeque::pop_front);
        let Some(event) = event else {
            return Err(ProviderError::Pending);
        };
        let outcome = ProviderOutcome::result(event);
        Ok(if resized {
            outcome.with_state(Value::Record(fields))
        } else {
            outcome
        })
    }

    fn push_resize_event(
        &self,
        object: ObjectId,
        columns: usize,
        rows: usize,
    ) -> Result<(), ProviderError> {
        let mut events = self.events.lock().map_err(|_| ProviderError::Unavailable)?;
        let queue = events.entry(object).or_default();
        if queue.len() >= 64 {
            queue.pop_front();
        }
        queue.push_back(Value::Record(BTreeMap::from([
            ("kind".to_owned(), Value::Text("resize".to_owned())),
            (
                "columns".to_owned(),
                Value::Integer(i64::try_from(columns).expect("bounded width")),
            ),
            (
                "rows".to_owned(),
                Value::Integer(i64::try_from(rows).expect("bounded height")),
            ),
        ])));
        Ok(())
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

fn render_terminal_frame(view: &mut TerminalRenderView<'_>, modes: &mut RenderModes) -> Vec<u8> {
    let mut frame = Vec::new();
    if !modes.initialized {
        frame.extend_from_slice(b"\x1b[0m\x1b[?25h\x1b[?7h");
        modes.initialized = true;
        modes.modes.cursor_visible = true;
        modes.modes.screen.autowrap = true;
    }
    if modes.modes.screen.alternate_screen != view.modes.screen.alternate_screen {
        frame.extend_from_slice(if view.modes.screen.alternate_screen {
            b"\x1b[?1049h"
        } else {
            b"\x1b[?1049l"
        });
        modes.modes.screen.alternate_screen = view.modes.screen.alternate_screen;
    }
    if modes.modes.cursor_visible != view.modes.cursor_visible {
        frame.extend_from_slice(if view.modes.cursor_visible {
            b"\x1b[?25h"
        } else {
            b"\x1b[?25l"
        });
        modes.modes.cursor_visible = view.modes.cursor_visible;
    }
    if modes.modes.screen.autowrap != view.modes.screen.autowrap {
        frame.extend_from_slice(if view.modes.screen.autowrap {
            b"\x1b[?7h"
        } else {
            b"\x1b[?7l"
        });
        modes.modes.screen.autowrap = view.modes.screen.autowrap;
    }
    if modes.modes.input.application_cursor != view.modes.input.application_cursor {
        frame.extend_from_slice(if view.modes.input.application_cursor {
            b"\x1b[?1h"
        } else {
            b"\x1b[?1l"
        });
        modes.modes.input.application_cursor = view.modes.input.application_cursor;
    }
    if modes.modes.input.bracketed_paste != view.modes.input.bracketed_paste {
        frame.extend_from_slice(if view.modes.input.bracketed_paste {
            b"\x1b[?2004h"
        } else {
            b"\x1b[?2004l"
        });
        modes.modes.input.bracketed_paste = view.modes.input.bracketed_paste;
    }
    if modes.modes.input.mouse_tracking != view.modes.input.mouse_tracking
        || modes.modes.input.sgr_mouse != view.modes.input.sgr_mouse
    {
        frame.extend_from_slice(b"\x1b[?1000l\x1b[?1002l\x1b[?1003l\x1b[?1006l");
        let tracking = match view.modes.input.mouse_tracking {
            MouseTracking::Disabled => None,
            MouseTracking::PressRelease => Some(b"\x1b[?1000h".as_slice()),
            MouseTracking::ButtonMotion => Some(b"\x1b[?1002h".as_slice()),
            MouseTracking::AnyMotion => Some(b"\x1b[?1003h".as_slice()),
        };
        if let Some(tracking) = tracking {
            frame.extend_from_slice(tracking);
        }
        if view.modes.input.sgr_mouse {
            frame.extend_from_slice(b"\x1b[?1006h");
        }
        modes.modes.input.mouse_tracking = view.modes.input.mouse_tracking;
        modes.modes.input.sgr_mouse = view.modes.input.sgr_mouse;
    }
    if modes.title != view.title {
        frame.extend_from_slice(b"\x1b]0;");
        frame.extend(view.title.bytes().filter(|byte| !byte.is_ascii_control()));
        frame.extend_from_slice(b"\x07");
        modes.title.clone_from(&view.title.to_owned());
    }

    for row in view.dirty_rows.drain(..) {
        if row >= view.rows {
            continue;
        }
        frame.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
        let mut previous_style = TerminalStyle::default();
        for cell in &view.cells[row] {
            if cell.style != previous_style {
                append_style(&mut frame, &cell.style);
                previous_style.clone_from(&cell.style);
            }
            if cell.width == 0 {
                continue;
            }
            if cell.text.is_empty() {
                frame.push(b' ');
            } else {
                frame.extend_from_slice(cell.text.as_bytes());
            }
        }
        frame.extend_from_slice(b"\x1b[0m\x1b[K");
    }
    let (row, column) = view.cursor;
    frame.extend_from_slice(format!("\x1b[{};{}H", row + 1, column + 1).as_bytes());
    frame
}

fn append_style(frame: &mut Vec<u8>, style: &TerminalStyle) {
    let mut codes = vec!["0".to_owned()];
    if style.bold {
        codes.push("1".to_owned());
    }
    if style.dim {
        codes.push("2".to_owned());
    }
    if style.italic {
        codes.push("3".to_owned());
    }
    if style.underline {
        codes.push("4".to_owned());
    }
    if style.blink {
        codes.push("5".to_owned());
    }
    if style.inverse {
        codes.push("7".to_owned());
    }
    if style.hidden {
        codes.push("8".to_owned());
    }
    if style.strikethrough {
        codes.push("9".to_owned());
    }
    append_color_code(&mut codes, style.foreground.as_ref(), true);
    append_color_code(&mut codes, style.background.as_ref(), false);
    frame.extend_from_slice(format!("\x1b[{}m", codes.join(";")).as_bytes());
}

fn append_color_code(codes: &mut Vec<String>, color: Option<&TerminalColor>, foreground: bool) {
    let Some(color) = color else { return };
    let base = if foreground { 38 } else { 48 };
    match color {
        TerminalColor::Indexed(index) => {
            codes.extend([base.to_string(), "5".to_owned(), index.to_string()]);
        }
        TerminalColor::Rgb(red, green, blue) => codes.extend([
            base.to_string(),
            "2".to_owned(),
            red.to_string(),
            green.to_string(),
            blue.to_string(),
        ]),
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
        fields
            .entry("parent_terminal".to_owned())
            .or_insert(Value::Null);
        fields
            .entry("foreground_process".to_owned())
            .or_insert(Value::Null);
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
                let Value::Record(mut child_fields) = self.create(&initial)? else {
                    unreachable!("Terminal create returns a Record")
                };
                let id = ObjectId::new();
                child_fields.insert(
                    "parent_terminal".to_owned(),
                    Value::Text(object.to_string()),
                );
                let request =
                    CreateObject::new(CORE_TERMINAL_TYPE, Value::Record(child_fields).encode()?)
                        .with_id(id)
                        .with_parent(object);
                self.routes
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .parents
                    .insert(id, object);
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
                let changed = positive_dimension(&fields, "columns", 80, 512)? != columns
                    || positive_dimension(&fields, "rows", 24, 256)? != rows;
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
                drop(screens);
                if changed {
                    self.push_resize_event(object, columns, rows)?;
                }
                Ok(ProviderOutcome::result(Value::Null).with_state(Value::Record(fields)))
            }
            _ => Err(ProviderError::InvalidArguments(
                "Terminal expects create(), configure(record), resize(columns, rows), or foreground()",
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
        let (terminal, restore) = self
            .routes
            .lock()
            .ok()
            .and_then(|mut routes| {
                let terminal = routes.process_terminals.remove(&process)?;
                routes.foregrounds.remove(&terminal);
                if routes.active_process != Some(process) {
                    return Some((Some(terminal), None));
                }
                let parent = routes
                    .parents
                    .get(&terminal)
                    .copied()
                    .unwrap_or(self.host_terminal);
                routes.active_terminal = parent;
                routes.active_process = routes.foregrounds.get(&parent).copied();
                Some((Some(terminal), Some(parent)))
            })
            .unwrap_or((None, None));
        if let (Some(manager), Some(terminal)) = (&self.manager, terminal)
            && let Ok(view) = manager.read(AccessContext::new(SYSTEM_SUBJECT), terminal)
            && let Ok(Value::Record(mut fields)) = Value::decode(view.state())
            && fields.get("foreground_process") == Some(&Value::Text(process.to_string()))
        {
            fields.insert("foreground_process".to_owned(), Value::Null);
            if let Ok(encoded) = Value::Record(fields).encode() {
                let mut transaction = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
                transaction
                    .expect(terminal, view.header().version)
                    .update_state(terminal, encoded);
                let _ = manager.commit(transaction);
            }
        }
        if let Some(parent) = restore {
            let (columns, rows) = self.terminal.size().map_or((80, 24), |(columns, rows)| {
                (
                    usize::from(columns).clamp(1, 512),
                    usize::from(rows).clamp(1, 256),
                )
            });
            if let Ok(mut screens) = self.screens.lock() {
                let screen = screens
                    .entry(parent)
                    .or_insert_with(|| TerminalScreen::new(columns, rows));
                screen.mark_all_dirty();
                let mut view = screen.render_view();
                if let Ok(mut modes) = self.render_modes.lock() {
                    let frame = render_terminal_frame(&mut view, &mut modes);
                    let _ = self.terminal.render(&frame);
                }
            }
        }
    }

    fn object_retired(&self, object: ObjectId) {
        let Ok(mut routes) = self.routes.lock() else {
            return;
        };
        let mut retired = BTreeSet::from([object]);
        loop {
            let descendants = routes
                .parents
                .iter()
                .filter(|(_, parent)| retired.contains(parent))
                .map(|(child, _)| *child)
                .filter(|child| !retired.contains(child))
                .collect::<Vec<_>>();
            if descendants.is_empty() {
                break;
            }
            retired.extend(descendants);
        }
        let processes = routes
            .process_terminals
            .iter()
            .filter(|(_, terminal)| retired.contains(terminal))
            .map(|(process, _)| *process)
            .collect::<Vec<_>>();
        let restore = retired.contains(&routes.active_terminal).then(|| {
            routes
                .parents
                .get(&object)
                .copied()
                .unwrap_or(self.host_terminal)
        });
        if let Some(parent) = restore {
            routes.active_terminal = parent;
            routes.active_process = routes.foregrounds.get(&parent).copied();
        }
        for process in &processes {
            routes.process_terminals.remove(process);
        }
        for terminal in &retired {
            routes.parents.remove(terminal);
            routes.foregrounds.remove(terminal);
        }
        if routes
            .active_process
            .is_some_and(|process| processes.contains(&process))
        {
            routes.active_process = None;
        }
        drop(routes);

        for process in processes {
            self.terminal.release_process_input(process);
        }
        if let Ok(mut canonical) = self.canonical_input.lock() {
            canonical.retain(|(_, terminal), _| !retired.contains(terminal));
        }
        if let Ok(mut events) = self.events.lock() {
            events.retain(|terminal, _| !retired.contains(terminal));
        }
        let (columns, rows) = self.terminal.size().map_or((80, 24), |(columns, rows)| {
            (
                usize::from(columns).clamp(1, 512),
                usize::from(rows).clamp(1, 256),
            )
        });
        let frame = self.screens.lock().ok().and_then(|mut screens| {
            for terminal in &retired {
                screens.remove(terminal);
            }
            restore.and_then(|parent| {
                let screen = screens
                    .entry(parent)
                    .or_insert_with(|| TerminalScreen::new(columns, rows));
                screen.mark_all_dirty();
                let mut view = screen.render_view();
                let mut modes = self.render_modes.lock().ok()?;
                Some(render_terminal_frame(&mut view, &mut modes))
            })
        });
        if let Some(frame) = frame {
            let _ = self.terminal.render(&frame);
        }
    }

    fn effect_recovery_policy(&self, capability: &str) -> EffectRecoveryPolicy {
        if capability == "foreground" {
            EffectRecoveryPolicy::RetryIdempotent
        } else {
            EffectRecoveryPolicy::Manual
        }
    }

    fn invoke_for_process(
        &self,
        process: ObjectId,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if capability == "wait_event" && arguments.is_empty() {
            return self.wait_event(process, object, state);
        }
        if capability != "foreground" || !arguments.is_empty() {
            return self.invoke(object, state, capability, arguments, ObjectId::new());
        }
        let mut fields = record(state)?.clone();
        self.activate(process, object, &mut fields)?;
        Ok(ProviderOutcome::result(Value::Null).with_state(Value::Record(fields)))
    }

    fn capabilities(&self) -> std::collections::BTreeSet<String> {
        ["create", "configure", "resize", "foreground", "wait_event"]
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

fn terminal_depth(parents: &BTreeMap<ObjectId, ObjectId>, mut terminal: ObjectId) -> usize {
    let mut visited = BTreeSet::new();
    let mut depth = 0;
    while visited.insert(terminal) {
        let Some(parent) = parents.get(&terminal).copied() else {
            break;
        };
        terminal = parent;
        depth += 1;
    }
    depth
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renderer_emits_styled_cells_and_terminal_modes() {
        let mut screen = TerminalScreen::new(8, 2);
        screen.write(
            b"\x1b[1;3;4;5;7;8;9;2;38;2;1;2;3;48;5;200mX\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h",
        );
        let mut modes = RenderModes::default();
        let mut view = screen.render_view();
        let frame = render_terminal_frame(&mut view, &mut modes);
        let rendered = String::from_utf8(frame).unwrap();
        for expected in [
            "\u{1b}[?1h",
            "\u{1b}[?2004h",
            "\u{1b}[?1002h",
            "\u{1b}[?1006h",
            "38;2;1;2;3",
            "48;5;200",
            "X",
        ] {
            assert!(rendered.contains(expected), "missing {expected:?}");
        }
    }

    #[test]
    fn renderer_restores_disabled_modes_without_redrawing_clean_rows() {
        let mut screen = TerminalScreen::new(8, 2);
        screen.write(b"\x1b[?1h\x1b[?2004h\x1b[?1002h\x1b[?1006h");
        let mut modes = RenderModes::default();
        let mut first = screen.render_view();
        let _ = render_terminal_frame(&mut first, &mut modes);
        screen.write(b"\x1b[?1l\x1b[?2004l\x1b[?1002l\x1b[?1006l");
        let mut second = screen.render_view();
        let frame = String::from_utf8(render_terminal_frame(&mut second, &mut modes)).unwrap();
        assert!(frame.contains("\u{1b}[?1l"));
        assert!(frame.contains("\u{1b}[?2004l"));
        assert!(frame.contains("\u{1b}[?1002l"));
        assert!(frame.contains("\u{1b}[?1006l"));
        assert_eq!(frame.matches("\u{1b}[1;1H").count(), 1);
    }
}
