#[derive(Debug)]
struct ConsoleObjectProvider {
    object: ObjectId,
    driver: Arc<dyn ConsoleProvider>,
    completed_this_boot: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
    secrets_this_boot: Mutex<BTreeMap<String, String>>,
}

impl ConsoleObjectProvider {
    fn invoke_console(
        &self,
        process: Option<ObjectId>,
        object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if object != self.object {
            return Err(ProviderError::UnsupportedCapability(capability.to_owned()));
        }
        let mut completed = self
            .completed_this_boot
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        if let Some(outcome) = completed.get(&effect) {
            return Ok(outcome.clone());
        }
        let outcome = match (capability, arguments) {
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
            ("read_line", []) => process
                .map_or_else(
                    || self.driver.try_read_line(),
                    |process| self.driver.try_read_line_for(process),
                )
                .map_err(ProviderError::Adapter)?
                .map(|line| ProviderOutcome::result(Value::Text(line)))
                .ok_or(ProviderError::Pending)?,
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
            _ => return Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        };
        completed.insert(effect, outcome.clone());
        Ok(outcome)
    }
}

impl ObjectProvider for ConsoleObjectProvider {
    fn type_id(&self) -> TypeId {
        CONSOLE_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Console Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        self.invoke_console(None, object, state, capability, arguments, effect)
    }

    fn invoke_for_process(
        &self,
        process: ObjectId,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        self.invoke_console(Some(process), object, state, capability, arguments, effect)
    }

    fn ephemeral_capabilities(&self) -> BTreeSet<String> {
        BTreeSet::from(["render".to_owned()])
    }

    fn invoke_ephemeral_for_process(
        &self,
        _process: ObjectId,
        object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        if object != self.object || capability != "render" {
            return Err(ProviderError::UnsupportedCapability(capability.to_owned()));
        }
        let frame = match arguments {
            [Value::Text(text)] => text.as_bytes(),
            [Value::Bytes(bytes)] => bytes.as_slice(),
            _ => {
                return Err(ProviderError::InvalidArguments(
                    "console.render expects one Text or Bytes frame",
                ));
            }
        };
        self.driver
            .render(frame)
            .map_err(ProviderError::Adapter)?;
        Ok(ProviderOutcome::result(Value::Null))
    }

    fn process_ended(&self, process: ObjectId) {
        self.driver.release_process(process);
    }

    fn capabilities(&self) -> BTreeSet<String> {
        BTreeSet::from([
            "print".to_owned(),
            "println".to_owned(),
            "render".to_owned(),
            "read_line".to_owned(),
            "read_secret".to_owned(),
            "size".to_owned(),
            "is_interactive".to_owned(),
        ])
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
