#[derive(Debug)]
pub(super) struct TerminalTransportProvider {
    pub(super) object: ObjectId,
    pub(super) driver: Arc<dyn TerminalProvider>,
    completed_this_boot: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
    secrets_this_boot: Mutex<BTreeMap<String, String>>,
}

impl TerminalTransportProvider {
    fn invoke_terminal(
        &self,
        process: Option<ObjectId>,
        object: ObjectId,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if object != self.object {
            return Err(ProviderError::UnsupportedCapability(capability.to_owned()));
        }
        if let Some(outcome) = self
            .completed_this_boot
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&effect)
            .cloned()
        {
            return Ok(outcome);
        }
        let outcome = match (capability, arguments) {
            ("create", [] | [Value::Record(_) | Value::Map(_)]) => {
                let initial = arguments.first().cloned().unwrap_or(Value::Null);
                let Value::Record(mut fields) = self.create(&initial)? else {
                    unreachable!("Terminal create returns a Record")
                };
                fields.insert("parent_terminal".to_owned(), Value::Text(object.to_string()));
                let request = CreateObject::new(
                    CORE_TERMINAL_TYPE,
                    Value::Record(fields).encode()?,
                )
                .with_parent(object);
                ProviderOutcome::result(Value::Text(request.id.to_string())).with_created(request)
            }
            ("print", [value]) => {
                self.driver
                    .print(&value.to_string())
                    .map_err(ProviderError::Adapter)?;
                ProviderOutcome::result(Value::Null)
            }
            ("println", [value]) => {
                self.driver
                    .println(&value.to_string())
                    .map_err(ProviderError::Adapter)?;
                ProviderOutcome::result(Value::Null)
            }
            ("size", []) => {
                let (columns, rows) = self.driver.size().map_err(ProviderError::Adapter)?;
                ProviderOutcome::result(Value::Record(BTreeMap::from([
                    ("columns".to_owned(), Value::Integer(i64::from(columns))),
                    ("rows".to_owned(), Value::Integer(i64::from(rows))),
                ])))
            }
            ("is_interactive", []) => {
                ProviderOutcome::result(Value::Bool(self.driver.is_interactive()))
            }
            ("read_line", []) => {
                let line = process
                    .map_or_else(
                        || self.driver.try_read_line(),
                        |process| self.driver.try_read_line_for(process),
                    )
                    .map_err(ProviderError::Adapter)?
                    .ok_or(ProviderError::Pending)?;
                ProviderOutcome::result(Value::Text(line))
            }
            ("read_secret", []) => {
                let secret = process
                    .map_or_else(
                        || self.driver.try_read_secret(),
                        |process| self.driver.try_read_secret_for(process),
                    )
                    .map_err(ProviderError::Adapter)?
                    .ok_or(ProviderError::Pending)?;
                let token = format!("secret:{effect}");
                self.secrets_this_boot
                    .lock()
                    .map_err(|_| ProviderError::Unavailable)?
                    .insert(token.clone(), secret);
                ProviderOutcome::result(Value::Text(token))
            }
            ("size" | "is_interactive" | "read_line" | "read_secret", _) => {
                return Err(ProviderError::InvalidArguments(
                    "Terminal method received invalid arguments",
                ));
            }
            _ => return Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        };
        self.completed_this_boot
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(effect, outcome.clone());
        Ok(outcome)
    }
}

impl ObjectProvider for TerminalTransportProvider {
    fn type_id(&self) -> TypeId {
        CORE_TERMINAL_TYPE
    }

    fn user_creatable(&self) -> bool {
        true
    }

    fn create(&self, initial: &Value) -> Result<Value, ProviderError> {
        let mut fields = match initial {
            Value::Null => BTreeMap::new(),
            Value::Record(fields) | Value::Map(fields) => fields.clone(),
            _ => {
                return Err(ProviderError::InvalidArguments(
                    "Terminal state must be a Record or null",
                ));
            }
        };
        fields.entry("columns".to_owned()).or_insert(Value::Integer(80));
        fields.entry("rows".to_owned()).or_insert(Value::Integer(24));
        fields
            .entry("input_mode".to_owned())
            .or_insert(Value::Text("raw".to_owned()));
        fields.entry("echo".to_owned()).or_insert(Value::Bool(false));
        fields
            .entry("parent_terminal".to_owned())
            .or_insert(Value::Null);
        fields
            .entry("foreground_process".to_owned())
            .or_insert(Value::Null);
        Ok(Value::Record(fields))
    }

    fn invoke(
        &self,
        object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        self.invoke_terminal(None, object, capability, arguments, effect)
    }

    fn invoke_for_process(
        &self,
        process: ObjectId,
        object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        self.invoke_terminal(Some(process), object, capability, arguments, effect)
    }

    fn process_ended(&self, process: ObjectId) {
        self.driver.release_process(process);
    }

    fn capabilities(&self) -> BTreeSet<String> {
        [
            "print",
            "println",
            "create",
            "output",
            "read_line",
            "read_secret",
            "size",
            "is_interactive",
        ]
        .into_iter()
        .map(str::to_owned)
        .collect()
    }

    fn ephemeral_capabilities(&self) -> BTreeSet<String> {
        BTreeSet::from(["output".to_owned()])
    }

    fn invoke_ephemeral_for_process(
        &self,
        _process: ObjectId,
        object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        if object != self.object || capability != "output" {
            return Err(ProviderError::UnsupportedCapability(capability.to_owned()));
        }
        let [Value::Bytes(bytes)] = arguments else {
            return Err(ProviderError::InvalidArguments(
                "Terminal output expects Bytes",
            ));
        };
        self.driver.render(bytes).map_err(ProviderError::Adapter)?;
        Ok(ProviderOutcome::result(Value::Integer(
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
        )))
    }

    fn resolve_secret(&self, token: &str) -> Result<Option<String>, ProviderError> {
        Ok(self
            .secrets_this_boot
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(token)
            .cloned())
    }
}

#[derive(Debug)]
struct TimeObjectProvider {
    started: Instant,
    completed: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
}

impl ObjectProvider for TimeObjectProvider {
    fn type_id(&self) -> TypeId {
        CORE_TIME_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Time Objects are published by system startup",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        let mut completed = self
            .completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        if let Some(outcome) = completed.get(&effect) {
            return Ok(outcome.clone());
        }
        let result = match (capability, arguments) {
            ("now", []) => {
                let millis = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map_err(|error| ProviderError::Adapter(error.to_string()))?
                    .as_millis();
                Value::Integer(i64::try_from(millis).map_err(|_| {
                    ProviderError::Adapter("Unix timestamp exceeds Praxis Integer".to_owned())
                })?)
            }
            ("monotonic", []) => Value::Integer(
                i64::try_from(self.started.elapsed().as_millis()).map_err(|_| {
                    ProviderError::Adapter("monotonic time exceeds Praxis Integer".to_owned())
                })?,
            ),
            _ => return Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        };
        let outcome = ProviderOutcome::result(result);
        completed.insert(effect, outcome.clone());
        Ok(outcome)
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["now", "monotonic"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}
