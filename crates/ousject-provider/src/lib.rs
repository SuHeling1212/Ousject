//! Generic domain-capability Providers and durable external Effect records.

use oms_runtime::CreateObject;
use oms_types::{CORE_EFFECT_TYPE, ObjectId, TypeId, Value, ValueError};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, RwLock};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderError {
    Missing(TypeId),
    Duplicate(TypeId),
    UnsupportedCapability(String),
    InvalidArguments(&'static str),
    Adapter(String),
    EffectState(&'static str),
    Pending,
    Value(ValueError),
    Unavailable,
}

impl fmt::Display for ProviderError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for ProviderError {}

impl From<ValueError> for ProviderError {
    fn from(error: ValueError) -> Self {
        Self::Value(error)
    }
}

#[derive(Debug, Clone)]
pub struct ProviderOutcome {
    pub result: Value,
    pub object_state: Option<Value>,
    pub created: Vec<CreateObject>,
}

impl ProviderOutcome {
    #[must_use]
    pub const fn result(result: Value) -> Self {
        Self {
            result,
            object_state: None,
            created: Vec::new(),
        }
    }

    #[must_use]
    pub fn with_state(mut self, state: Value) -> Self {
        self.object_state = Some(state);
        self
    }

    #[must_use]
    pub fn with_created(mut self, object: CreateObject) -> Self {
        self.created.push(object);
        self
    }
}

pub trait ObjectProvider: fmt::Debug + Send + Sync {
    fn type_id(&self) -> TypeId;

    /// Whether ordinary Praxis code may request new Objects from this Provider.
    /// Hardware discovery Providers keep the default `false`.
    fn user_creatable(&self) -> bool {
        false
    }

    /// Validates and initializes persistent state for a Provider-only Object.
    ///
    /// # Errors
    ///
    /// Returns an adapter or argument error when creation is not valid.
    fn create(&self, initial: &Value) -> Result<Value, ProviderError>;

    /// Performs one domain capability. `effect` is a stable idempotency key.
    ///
    /// # Errors
    ///
    /// Returns an explicit Provider error; the durable Effect remains available
    /// for inspection/retry.
    fn invoke(
        &self,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError>;

    /// Performs a capability on behalf of a specific Process. Providers that
    /// need per-Process leases (for example exclusive keyboard input) can
    /// override this method; ordinary Providers remain source-compatible.
    ///
    /// # Errors
    ///
    /// Returns the same provider-specific errors as [`Self::invoke`].
    fn invoke_for_process(
        &self,
        _process: ObjectId,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        self.invoke(object, state, capability, arguments, effect)
    }

    /// Capabilities in this set are explicitly transient: they do not create
    /// a durable Effect record and must not mutate persistent Object state.
    fn ephemeral_capabilities(&self) -> BTreeSet<String> {
        BTreeSet::new()
    }

    /// Performs a transient capability for a Process. The VM commits the
    /// Process at its normal execution-slice boundary instead of persisting an
    /// Effect for this call.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific error if the transient operation fails.
    fn invoke_ephemeral_for_process(
        &self,
        _process: ObjectId,
        object: ObjectId,
        state: &Value,
        capability: &str,
        arguments: &[Value],
    ) -> Result<ProviderOutcome, ProviderError> {
        let _ = (object, state, arguments);
        Err(ProviderError::UnsupportedCapability(capability.to_owned()))
    }

    /// Releases any boot-scoped resources leased by a Process that ended.
    fn process_ended(&self, _process: ObjectId) {}

    fn capabilities(&self) -> BTreeSet<String>;

    /// Resolves a boot-scoped opaque secret token. Providers that do not own
    /// secret input keep the default `None` implementation.
    ///
    /// # Errors
    ///
    /// Returns a Provider error if its protected in-memory secret store is
    /// unavailable.
    fn resolve_secret(&self, _token: &str) -> Result<Option<String>, ProviderError> {
        Ok(None)
    }
}

#[derive(Debug, Default)]
pub struct ProviderRegistry {
    providers: RwLock<BTreeMap<TypeId, Arc<dyn ObjectProvider>>>,
}

impl ProviderRegistry {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Registers exactly one Provider for a stable Type.
    ///
    /// # Errors
    ///
    /// Returns `Duplicate` when a Provider is already active for that Type.
    pub fn register(&self, provider: Arc<dyn ObjectProvider>) -> Result<(), ProviderError> {
        let type_id = provider.type_id();
        let mut providers = self
            .providers
            .write()
            .map_err(|_| ProviderError::Unavailable)?;
        if providers.contains_key(&type_id) {
            return Err(ProviderError::Duplicate(type_id));
        }
        providers.insert(type_id, provider);
        Ok(())
    }

    /// Returns the active Provider for a Type.
    ///
    /// # Errors
    ///
    /// Returns `Missing` or `Unavailable` when dispatch is impossible.
    pub fn get(&self, type_id: TypeId) -> Result<Arc<dyn ObjectProvider>, ProviderError> {
        self.providers
            .read()
            .map_err(|_| ProviderError::Unavailable)?
            .get(&type_id)
            .cloned()
            .ok_or(ProviderError::Missing(type_id))
    }

    /// Returns the stable Type IDs that have an active Provider this boot.
    /// A cloned snapshot avoids holding the registry lock while callers work.
    ///
    /// # Errors
    ///
    /// Returns `Unavailable` if the registry lock is poisoned.
    pub fn types(&self) -> Result<Vec<TypeId>, ProviderError> {
        Ok(self
            .providers
            .read()
            .map_err(|_| ProviderError::Unavailable)?
            .keys()
            .copied()
            .collect())
    }

    /// Notifies Providers that a Process has ended so they can release leases.
    ///
    /// # Errors
    ///
    /// Returns `Unavailable` if the registry lock is poisoned.
    pub fn process_ended(&self, process: ObjectId) -> Result<(), ProviderError> {
        let providers = self
            .providers
            .read()
            .map_err(|_| ProviderError::Unavailable)?;
        for provider in providers.values() {
            provider.process_ended(process);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectStatus {
    Pending,
    Completed,
    Failed,
}

impl EffectStatus {
    const fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EffectRecord {
    pub process: ObjectId,
    pub target: ObjectId,
    pub token_position: u32,
    pub capability: String,
    pub arguments: Vec<Value>,
    pub status: EffectStatus,
    pub result: Option<Value>,
    pub error: Option<String>,
}

impl EffectRecord {
    #[must_use]
    pub fn pending(
        process: ObjectId,
        target: ObjectId,
        token_position: u32,
        capability: impl Into<String>,
        arguments: Vec<Value>,
    ) -> Self {
        Self {
            process,
            target,
            token_position,
            capability: capability.into(),
            arguments,
            status: EffectStatus::Pending,
            result: None,
            error: None,
        }
    }

    #[must_use]
    pub fn complete(mut self, result: Value) -> Self {
        self.status = EffectStatus::Completed;
        self.result = Some(result);
        self.error = None;
        self
    }

    #[must_use]
    pub fn fail(mut self, error: impl Into<String>) -> Self {
        self.status = EffectStatus::Failed;
        self.result = None;
        self.error = Some(error.into());
        self
    }

    /// Encodes the Effect as the common recursive Value format.
    ///
    /// # Errors
    ///
    /// Returns a Value encoding error if a configured limit is exceeded.
    pub fn encode(&self) -> Result<Vec<u8>, ProviderError> {
        Ok(self.value().encode()?)
    }

    #[must_use]
    pub fn value(&self) -> Value {
        let mut fields = BTreeMap::from([
            ("process".to_owned(), Value::Text(self.process.to_string())),
            ("target".to_owned(), Value::Text(self.target.to_string())),
            (
                "token_position".to_owned(),
                Value::Integer(i64::from(self.token_position)),
            ),
            (
                "capability".to_owned(),
                Value::Text(self.capability.clone()),
            ),
            ("arguments".to_owned(), Value::Array(self.arguments.clone())),
            (
                "status".to_owned(),
                Value::Text(self.status.name().to_owned()),
            ),
        ]);
        fields.insert(
            "result".to_owned(),
            self.result.clone().unwrap_or(Value::Null),
        );
        fields.insert(
            "error".to_owned(),
            self.error.clone().map_or(Value::Null, Value::Text),
        );
        Value::Record(fields)
    }

    /// Decodes and validates a durable Effect record.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid Value bytes or malformed fields.
    pub fn decode(bytes: &[u8]) -> Result<Self, ProviderError> {
        let value = Value::decode(bytes)?;
        let Value::Record(fields) = value else {
            return Err(ProviderError::EffectState("Effect must be a Record"));
        };
        let process = text(&fields, "process")?
            .parse()
            .map_err(|_| ProviderError::EffectState("invalid Effect Process"))?;
        let target = text(&fields, "target")?
            .parse()
            .map_err(|_| ProviderError::EffectState("invalid Effect target"))?;
        let token_position = u32::try_from(integer(&fields, "token_position")?)
            .map_err(|_| ProviderError::EffectState("invalid Effect token position"))?;
        let Some(Value::Array(arguments)) = fields.get("arguments") else {
            return Err(ProviderError::EffectState("invalid Effect arguments"));
        };
        let status = match text(&fields, "status")? {
            "pending" => EffectStatus::Pending,
            "completed" => EffectStatus::Completed,
            "failed" => EffectStatus::Failed,
            _ => return Err(ProviderError::EffectState("invalid Effect status")),
        };
        Ok(Self {
            process,
            target,
            token_position,
            capability: text(&fields, "capability")?.to_owned(),
            arguments: arguments.clone(),
            status,
            result: match fields.get("result") {
                None | Some(Value::Null) => None,
                Some(value) => Some(value.clone()),
            },
            error: match fields.get("error") {
                None | Some(Value::Null) => None,
                Some(Value::Text(value)) => Some(value.clone()),
                Some(_) => return Err(ProviderError::EffectState("invalid Effect error")),
            },
        })
    }

    /// Builds a Provider-only `core.effect` child Object request.
    ///
    /// # Errors
    ///
    /// Returns an error if the Effect Value cannot be encoded.
    pub fn create_object(&self, id: ObjectId) -> Result<CreateObject, ProviderError> {
        Ok(CreateObject::new(CORE_EFFECT_TYPE, self.encode()?)
            .with_id(id)
            .with_parent(self.process))
    }
}

fn text<'a>(fields: &'a BTreeMap<String, Value>, key: &str) -> Result<&'a str, ProviderError> {
    let Some(Value::Text(value)) = fields.get(key) else {
        return Err(ProviderError::EffectState("missing Effect Text field"));
    };
    Ok(value)
}

fn integer(fields: &BTreeMap<String, Value>, key: &str) -> Result<i64, ProviderError> {
    let Some(Value::Integer(value)) = fields.get(key) else {
        return Err(ProviderError::EffectState("missing Effect Integer field"));
    };
    Ok(*value)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effect_round_trips_without_losing_request_or_result() {
        let effect = EffectRecord::pending(
            ObjectId::new(),
            ObjectId::new(),
            42,
            "send",
            vec![Value::Bytes(vec![1, 2, 3])],
        )
        .complete(Value::Integer(3));
        assert_eq!(EffectRecord::decode(&effect.encode().unwrap()), Ok(effect));
    }
}
