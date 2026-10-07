use super::super::{
    BTreeMap, BTreeSet, Mutex, ObjectId, ObjectProvider, ProviderError, ProviderOutcome, Value,
};

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
