use super::super::{
    BTreeMap, BTreeSet, DEVICE_DISPLAY_TYPE, Mutex, ObjectId, ObjectProvider, ProviderError,
    ProviderOutcome, Value, Write,
};
use super::adapter_error;

#[derive(Debug)]
pub(crate) struct CachedProvider<P> {
    inner: P,
    completed: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
}

impl<P> CachedProvider<P> {
    pub(crate) fn new(inner: P) -> Self {
        Self {
            inner,
            completed: Mutex::new(BTreeMap::new()),
        }
    }
}

impl<P: ObjectProvider> ObjectProvider for CachedProvider<P> {
    fn type_id(&self) -> oms_types::TypeId {
        self.inner.type_id()
    }

    fn user_creatable(&self) -> bool {
        self.inner.user_creatable()
    }

    fn create(&self, initial: &Value) -> Result<Value, ProviderError> {
        self.inner.create(initial)
    }

    fn invoke(
        &self,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if let Some(outcome) = self
            .completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&effect)
            .cloned()
        {
            return Ok(outcome);
        }
        let outcome = self
            .inner
            .invoke(object, state, capability, arguments, effect)?;
        self.completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(effect, outcome.clone());
        Ok(outcome)
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
        if let Some(outcome) = self
            .completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&effect)
            .cloned()
        {
            return Ok(outcome);
        }
        let outcome = self
            .inner
            .invoke_for_process(process, object, state, capability, arguments, effect)?;
        self.completed
            .lock()
            .map_err(|_| ProviderError::Unavailable)?
            .insert(effect, outcome.clone());
        Ok(outcome)
    }

    fn ephemeral_capabilities(&self) -> BTreeSet<String> {
        self.inner.ephemeral_capabilities()
    }

    fn invoke_ephemeral_for_process(
        &self,
        process: ObjectId,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        self.inner
            .invoke_ephemeral_for_process(process, object, state, capability, arguments)
    }

    fn process_ended(&self, process: ObjectId) {
        self.inner.process_ended(process);
    }

    fn capabilities(&self) -> BTreeSet<String> {
        self.inner.capabilities()
    }
}

#[derive(Debug)]
pub(crate) struct HostDisplayProvider;

impl ObjectProvider for HostDisplayProvider {
    fn type_id(&self) -> oms_types::TypeId {
        DEVICE_DISPLAY_TYPE
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Err(ProviderError::InvalidArguments(
            "Display Objects are published by hardware discovery",
        ))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        _effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        let outcome = match (capability, arguments) {
            ("present", [Value::Text(frame)]) => {
                write!(std::io::stdout().lock(), "{frame}").map_err(adapter_error)?;
                std::io::stdout().lock().flush().map_err(adapter_error)?;
                ProviderOutcome::result(Value::Null)
            }
            ("present", [Value::Bytes(frame)]) => {
                std::io::stdout()
                    .lock()
                    .write_all(frame)
                    .and_then(|()| std::io::stdout().lock().flush())
                    .map_err(adapter_error)?;
                ProviderOutcome::result(Value::Null)
            }
            ("configure", [configuration]) => {
                return Ok(
                    ProviderOutcome::result(Value::Null).with_state(Value::Record(BTreeMap::from(
                        [
                            (
                                "provider".to_owned(),
                                Value::Text("linux.terminal.display".to_owned()),
                            ),
                            ("configuration".to_owned(), configuration.clone()),
                        ],
                    ))),
                );
            }
            _ => return Err(ProviderError::UnsupportedCapability(capability.to_owned())),
        };
        Ok(outcome.with_state(state.clone()))
    }

    fn capabilities(&self) -> BTreeSet<String> {
        ["present", "configure"]
            .into_iter()
            .map(str::to_owned)
            .collect()
    }
}
