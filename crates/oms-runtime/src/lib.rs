//! Object Management System core.
//!
//! Transactions lock participating state in a stable global order, validate a
//! complete candidate, make it durable, and only then publish it. Readers can
//! therefore never observe a partially published cross-shard transaction.

use oms_shard::FixedDirectory;
use oms_types::{
    CORE_AUTHENTICATION_TYPE, CORE_BYTES_TYPE, CORE_CHANNEL_TYPE, CORE_COLLECTION_TYPE,
    CORE_COMPILER_TYPE, CORE_CONSOLE_TYPE, CORE_EFFECT_TYPE, CORE_INSTANCE_TYPE, CORE_MATH_TYPE,
    CORE_NAMESPACE_TYPE, CORE_OBJECT_STORE_TYPE, CORE_PROCESS_TYPE, CORE_PROGRAM_TYPE,
    CORE_PROVIDER_REGISTRY_TYPE, CORE_SCHEDULER_TYPE, CORE_SESSION_TYPE, CORE_SYSTEM_TYPE,
    CORE_TEXT_TYPE, CORE_TIME_TYPE, CORE_TYPE_REGISTRY_TYPE, CORE_USER_REGISTRY_TYPE,
    CORE_VALUE_TYPE, Capability, DEVICE_BLOCK_STORAGE_TYPE, DEVICE_DISPLAY_TYPE,
    DEVICE_KEYBOARD_TYPE, DEVICE_SENSOR_TYPE, LifecycleState, NET_ENDPOINT_TYPE, NET_RESOLVER_TYPE,
    ObjectHeader, ObjectId, ObjectVersion, OmsError, SYSTEM_SUBJECT, ShardId, SubjectId,
    TYPE_DESCRIPTOR_TYPE, TransactionId, TypeId, Value,
};
use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const SNAPSHOT_MAGIC: &[u8; 4] = b"OMS0";
const WAL_MAGIC: &[u8; 4] = b"OMW0";
const MANIFEST_MAGIC: &[u8; 4] = b"OMG0";
const RETIREMENT_TIME_EXTENSION: &[u8; 4] = b"RTM0";
const MAX_SNAPSHOT_ITEMS: usize = 16 * 1024 * 1024;
const CHECKPOINT_COMMIT_INTERVAL: u64 = 64;
const TOMBSTONE_RETENTION_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
const TOMBSTONE_REAPER_RETRY: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AccessContext {
    pub subject: SubjectId,
}

impl AccessContext {
    #[must_use]
    pub const fn new(subject: SubjectId) -> Self {
        Self { subject }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSchema {
    Any,
    Text,
    Bytes,
    Collection,
    Record,
}

impl ValueSchema {
    const fn accepts(self, value: &Value) -> bool {
        match self {
            Self::Any => true,
            Self::Text => matches!(value, Value::Text(_)),
            Self::Bytes => matches!(value, Value::Bytes(_)),
            Self::Collection => matches!(value, Value::Array(_) | Value::Map(_)),
            Self::Record => matches!(value, Value::Record(_)),
        }
    }

    const fn name(self) -> &'static str {
        match self {
            Self::Any => "any",
            Self::Text => "text",
            Self::Bytes => "bytes",
            Self::Collection => "array or map",
            Self::Record => "record",
        }
    }
}

fn normalize_value(schema: ValueSchema, value: &Value) -> Value {
    match (schema, value) {
        (ValueSchema::Record, Value::Map(entries)) => Value::Record(entries.clone()),
        _ => value.clone(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CreationPolicy {
    Public,
    ProviderOnly,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeDescriptor {
    pub id: TypeId,
    pub name: String,
    pub schema: ValueSchema,
    pub creation: CreationPolicy,
    pub capabilities: BTreeSet<Capability>,
    pub domain_capabilities: BTreeSet<String>,
}

#[derive(Debug, Clone)]
struct TypeRegistry {
    by_id: BTreeMap<TypeId, TypeDescriptor>,
    by_name: BTreeMap<String, TypeId>,
}

impl TypeRegistry {
    // Keeping the built-in descriptor table together makes stable IDs and
    // policies auditable as one unit.
    #[allow(clippy::too_many_lines)]
    fn builtins() -> Self {
        let descriptors = [
            type_descriptor(
                TYPE_DESCRIPTOR_TYPE,
                "core.type",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                CORE_VALUE_TYPE,
                "core.value",
                ValueSchema::Any,
                CreationPolicy::Public,
                &[
                    "slice",
                    "find",
                    "contains",
                    "split",
                    "replace_all",
                    "trim",
                    "lower",
                    "upper",
                ],
            ),
            type_descriptor(
                CORE_TEXT_TYPE,
                "core.text",
                ValueSchema::Text,
                CreationPolicy::Public,
                &[
                    "slice",
                    "find",
                    "contains",
                    "split",
                    "replace_all",
                    "trim",
                    "lower",
                    "upper",
                ],
            ),
            type_descriptor(
                CORE_BYTES_TYPE,
                "core.bytes",
                ValueSchema::Bytes,
                CreationPolicy::Public,
                &[],
            ),
            type_descriptor(
                CORE_COLLECTION_TYPE,
                "core.collection",
                ValueSchema::Collection,
                CreationPolicy::Public,
                &[],
            ),
            type_descriptor(
                CORE_CHANNEL_TYPE,
                "core.channel",
                ValueSchema::Collection,
                CreationPolicy::Public,
                &["send", "receive", "wait"],
            ),
            type_descriptor(
                CORE_NAMESPACE_TYPE,
                "core.namespace",
                ValueSchema::Record,
                CreationPolicy::Public,
                &["resolve", "bind", "unbind"],
            ),
            type_descriptor(
                CORE_INSTANCE_TYPE,
                "core.instance",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                CORE_PROGRAM_TYPE,
                "core.program",
                ValueSchema::Bytes,
                CreationPolicy::ProviderOnly,
                &["execute"],
            ),
            type_descriptor(
                CORE_PROCESS_TYPE,
                "core.process",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "start",
                    "wait",
                    "suspend",
                    "resume",
                    "terminate",
                    "bindings",
                ],
            ),
            type_descriptor(
                oms_types::CORE_USER_TYPE,
                "core.user",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                CORE_SESSION_TYPE,
                "core.session",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["revoke"],
            ),
            type_descriptor(
                CORE_EFFECT_TYPE,
                "core.effect",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["status", "result"],
            ),
            type_descriptor(
                CORE_CONSOLE_TYPE,
                "core.console",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "print",
                    "println",
                    "read_line",
                    "read_secret",
                    "size",
                    "is_interactive",
                ],
            ),
            type_descriptor(
                CORE_SYSTEM_TYPE,
                "core.system",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["status", "health_check", "shutdown", "restart"],
            ),
            type_descriptor(
                CORE_AUTHENTICATION_TYPE,
                "core.authentication",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "local_initialized",
                    "initialize_local",
                    "login",
                    "logout",
                    "current_user",
                    "change_password",
                ],
            ),
            type_descriptor(
                CORE_USER_REGISTRY_TYPE,
                "core.user_registry",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["create_user", "users", "disable_user"],
            ),
            type_descriptor(
                CORE_SCHEDULER_TYPE,
                "core.scheduler",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                CORE_COMPILER_TYPE,
                "core.compiler",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["compile", "validate", "disassemble"],
            ),
            type_descriptor(
                CORE_TYPE_REGISTRY_TYPE,
                "core.type_registry",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["register", "types", "descriptor"],
            ),
            type_descriptor(
                CORE_PROVIDER_REGISTRY_TYPE,
                "core.provider_registry",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["providers", "devices"],
            ),
            type_descriptor(
                CORE_OBJECT_STORE_TYPE,
                "core.object_store",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["stats", "health_check", "effects"],
            ),
            type_descriptor(
                CORE_MATH_TYPE,
                "core.math",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "abs",
                    "min",
                    "max",
                    "clamp",
                    "sqrt",
                    "pow",
                    "floor",
                    "ceil",
                    "round",
                    "trunc",
                    "sin",
                    "cos",
                    "tan",
                    "atan2",
                    "log",
                    "log2",
                    "log10",
                    "exp",
                    "hypot",
                    "random",
                    "random_integer",
                ],
            ),
            type_descriptor(
                CORE_TIME_TYPE,
                "core.time",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["now", "monotonic", "sleep"],
            ),
            type_descriptor(
                NET_RESOLVER_TYPE,
                "net.resolver",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["resolve"],
            ),
            type_descriptor(
                NET_ENDPOINT_TYPE,
                "net.endpoint",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["connect", "listen", "accept", "send", "receive", "close"],
            ),
            type_descriptor(
                DEVICE_DISPLAY_TYPE,
                "device.display",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["present", "configure"],
            ),
            type_descriptor(
                DEVICE_SENSOR_TYPE,
                "device.sensor",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["sample", "calibrate"],
            ),
            type_descriptor(
                DEVICE_KEYBOARD_TYPE,
                "device.keyboard",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["capture", "release", "next_event", "poll_event"],
            ),
            type_descriptor(
                DEVICE_BLOCK_STORAGE_TYPE,
                "device.block_storage",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["load_block", "store_block"],
            ),
        ];
        let mut by_id = BTreeMap::new();
        let mut by_name = BTreeMap::new();
        for descriptor in descriptors {
            by_name.insert(descriptor.name.clone(), descriptor.id);
            by_id.insert(descriptor.id, descriptor);
        }
        Self { by_id, by_name }
    }

    fn by_name(&self, name: &str) -> Result<&TypeDescriptor, OmsError> {
        let id = self
            .by_name
            .get(name)
            .ok_or_else(|| OmsError::UnknownTypeName(name.to_owned()))?;
        self.by_id.get(id).ok_or(OmsError::UnknownType(*id))
    }

    fn by_id(&self, id: TypeId) -> Result<&TypeDescriptor, OmsError> {
        self.by_id.get(&id).ok_or(OmsError::UnknownType(id))
    }

    fn all(&self) -> Vec<TypeDescriptor> {
        self.by_id.values().cloned().collect()
    }
}

fn type_descriptor(
    id: TypeId,
    name: &str,
    schema: ValueSchema,
    creation: CreationPolicy,
    domain_capabilities: &[&str],
) -> TypeDescriptor {
    TypeDescriptor {
        id,
        name: name.to_owned(),
        schema,
        creation,
        capabilities: all_capabilities(),
        domain_capabilities: domain_capabilities
            .iter()
            .map(|value| (*value).to_owned())
            .collect(),
    }
}

fn encode_type_descriptor(descriptor: &TypeDescriptor) -> Result<Vec<u8>, OmsError> {
    Value::Record(BTreeMap::from([
        ("id".to_owned(), Value::Text(descriptor.id.to_string())),
        ("name".to_owned(), Value::Text(descriptor.name.clone())),
        (
            "schema".to_owned(),
            Value::Text(descriptor.schema.name().to_owned()),
        ),
        (
            "creation".to_owned(),
            Value::Text(
                match descriptor.creation {
                    CreationPolicy::Public => "public",
                    CreationPolicy::ProviderOnly => "provider_only",
                }
                .to_owned(),
            ),
        ),
        (
            "domain_capabilities".to_owned(),
            Value::Array(
                descriptor
                    .domain_capabilities
                    .iter()
                    .cloned()
                    .map(Value::Text)
                    .collect(),
            ),
        ),
    ]))
    .encode()
    .map_err(|error| OmsError::InvalidValue(error.to_string()))
}

fn decode_type_descriptor(bytes: &[u8]) -> Result<TypeDescriptor, OmsError> {
    let value = Value::decode(bytes).map_err(|error| OmsError::InvalidValue(error.to_string()))?;
    let Value::Record(fields) = value else {
        return Err(OmsError::InvalidOperation(
            "Type Descriptor state must be a Record",
        ));
    };
    let text = |name: &str| match fields.get(name) {
        Some(Value::Text(value)) => Ok(value.as_str()),
        _ => Err(OmsError::InvalidOperation(
            "Type Descriptor has an invalid Text field",
        )),
    };
    let id = text("id")?
        .parse()
        .map_err(|_| OmsError::InvalidOperation("Type Descriptor has an invalid TypeId"))?;
    let schema = match text("schema")? {
        "any" => ValueSchema::Any,
        "text" => ValueSchema::Text,
        "bytes" => ValueSchema::Bytes,
        "array or map" => ValueSchema::Collection,
        "record" => ValueSchema::Record,
        _ => {
            return Err(OmsError::InvalidOperation(
                "Type Descriptor has an invalid schema",
            ));
        }
    };
    let creation = match text("creation")? {
        "public" => CreationPolicy::Public,
        "provider_only" => CreationPolicy::ProviderOnly,
        _ => {
            return Err(OmsError::InvalidOperation(
                "Type Descriptor has an invalid creation policy",
            ));
        }
    };
    let Some(Value::Array(capabilities)) = fields.get("domain_capabilities") else {
        return Err(OmsError::InvalidOperation(
            "Type Descriptor capabilities must be an Array",
        ));
    };
    let domain_capabilities = capabilities
        .iter()
        .map(|value| match value {
            Value::Text(value) if !value.is_empty() => Ok(value.clone()),
            _ => Err(OmsError::InvalidOperation(
                "Type Descriptor capability must be non-empty Text",
            )),
        })
        .collect::<Result<_, _>>()?;
    Ok(TypeDescriptor {
        id,
        name: text("name")?.to_owned(),
        schema,
        creation,
        capabilities: all_capabilities(),
        domain_capabilities,
    })
}

#[derive(Debug, Clone)]
pub struct CreateSpec {
    pub type_name: String,
    pub value: Value,
    pub parent: Option<ObjectId>,
    pub links: BTreeMap<String, ObjectId>,
}

impl CreateSpec {
    #[must_use]
    pub fn new(type_name: impl Into<String>, value: Value) -> Self {
        Self {
            type_name: type_name.into(),
            value,
            parent: None,
            links: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_parent(mut self, parent: ObjectId) -> Self {
        self.parent = Some(parent);
        self
    }

    #[must_use]
    pub fn with_link(mut self, name: impl Into<String>, target: ObjectId) -> Self {
        self.links.insert(name.into(), target);
        self
    }
}

#[derive(Debug, Clone, Default)]
pub struct ObjectQuery {
    pub type_id: Option<TypeId>,
    pub parent: Option<ObjectId>,
    pub capability: Option<Capability>,
    pub domain_capability: Option<String>,
}

impl ObjectQuery {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            type_id: None,
            parent: None,
            capability: None,
            domain_capability: None,
        }
    }

    #[must_use]
    pub const fn with_type(mut self, type_id: TypeId) -> Self {
        self.type_id = Some(type_id);
        self
    }

    #[must_use]
    pub const fn with_parent(mut self, parent: ObjectId) -> Self {
        self.parent = Some(parent);
        self
    }

    #[must_use]
    pub const fn with_capability(mut self, capability: Capability) -> Self {
        self.capability = Some(capability);
        self
    }

    #[must_use]
    pub fn with_domain_capability(mut self, capability: impl Into<String>) -> Self {
        self.domain_capability = Some(capability.into());
        self
    }
}

#[derive(Debug, Clone)]
pub struct CreateObject {
    pub id: ObjectId,
    pub type_id: TypeId,
    pub parent: Option<ObjectId>,
    pub state: Vec<u8>,
    pub capabilities: BTreeSet<Capability>,
    pub links: BTreeMap<String, ObjectId>,
    pub initial_grants: BTreeMap<SubjectId, BTreeSet<Capability>>,
}

impl CreateObject {
    #[must_use]
    pub fn new(type_id: TypeId, state: impl Into<Vec<u8>>) -> Self {
        Self {
            id: ObjectId::new(),
            type_id,
            parent: None,
            state: state.into(),
            capabilities: all_capabilities(),
            links: BTreeMap::new(),
            initial_grants: BTreeMap::new(),
        }
    }

    #[must_use]
    pub fn with_id(mut self, id: ObjectId) -> Self {
        self.id = id;
        self
    }

    #[must_use]
    pub fn with_parent(mut self, parent: ObjectId) -> Self {
        self.parent = Some(parent);
        self
    }

    #[must_use]
    pub fn with_link(mut self, name: impl Into<String>, target: ObjectId) -> Self {
        self.links.insert(name.into(), target);
        self
    }

    /// Adds a capability grant that becomes visible atomically with creation.
    #[must_use]
    pub fn with_grant(mut self, subject: SubjectId, capability: Capability) -> Self {
        self.initial_grants
            .entry(subject)
            .or_default()
            .insert(capability);
        self
    }
}

#[derive(Debug, Clone)]
struct AccessPolicy {
    owner: SubjectId,
    grants: BTreeMap<SubjectId, BTreeSet<Capability>>,
}

impl AccessPolicy {
    fn allows(&self, subject: SubjectId, capability: Capability) -> bool {
        subject == SYSTEM_SUBJECT
            || self.owner == subject
            || self
                .grants
                .get(&subject)
                .is_some_and(|capabilities| capabilities.contains(&capability))
    }
}

#[derive(Debug, Clone)]
struct ObjectRecord {
    header: ObjectHeader,
    retired_at_unix_ms: Option<u64>,
    state: Arc<[u8]>,
    children: BTreeSet<ObjectId>,
    links: BTreeMap<String, ObjectId>,
    capabilities: BTreeSet<Capability>,
    policy: AccessPolicy,
}

impl ObjectRecord {
    fn require(&self, context: AccessContext, capability: Capability) -> Result<(), OmsError> {
        if self.header.lifecycle == LifecycleState::Tombstoned && capability != Capability::Inspect
        {
            return Err(OmsError::InvalidLifecycle {
                object: self.header.id,
                state: self.header.lifecycle,
            });
        }
        if !self.capabilities.contains(&capability)
            || !self.policy.allows(context.subject, capability)
        {
            return Err(OmsError::Denied {
                object: self.header.id,
                capability,
            });
        }
        Ok(())
    }

    fn view(&self) -> ObjectView {
        ObjectView {
            header: self.header.clone(),
            state: Arc::clone(&self.state),
            children: Arc::new(self.children.clone()),
            links: Arc::new(self.links.clone()),
            capabilities: Arc::new(self.capabilities.clone()),
            owner: self.policy.owner,
            grants: Arc::new(self.policy.grants.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ObjectView {
    header: ObjectHeader,
    state: Arc<[u8]>,
    children: Arc<BTreeSet<ObjectId>>,
    links: Arc<BTreeMap<String, ObjectId>>,
    capabilities: Arc<BTreeSet<Capability>>,
    owner: SubjectId,
    grants: Arc<BTreeMap<SubjectId, BTreeSet<Capability>>>,
}

impl ObjectView {
    #[must_use]
    pub const fn header(&self) -> &ObjectHeader {
        &self.header
    }

    #[must_use]
    pub fn state(&self) -> &[u8] {
        &self.state
    }

    #[must_use]
    pub fn children(&self) -> &BTreeSet<ObjectId> {
        &self.children
    }

    #[must_use]
    pub fn links(&self) -> &BTreeMap<String, ObjectId> {
        &self.links
    }

    #[must_use]
    pub fn capabilities(&self) -> &BTreeSet<Capability> {
        &self.capabilities
    }

    #[must_use]
    pub const fn owner(&self) -> SubjectId {
        self.owner
    }

    #[must_use]
    pub fn grants(&self) -> &BTreeMap<SubjectId, BTreeSet<Capability>> {
        &self.grants
    }
}

#[derive(Debug, Clone)]
enum Operation {
    Create(CreateObject),
    UpdateState {
        object: ObjectId,
        state: Vec<u8>,
    },
    SetLink {
        source: ObjectId,
        name: String,
        target: ObjectId,
    },
    RemoveLink {
        source: ObjectId,
        name: String,
    },
    Reparent {
        child: ObjectId,
        new_parent: Option<ObjectId>,
    },
    Grant {
        object: ObjectId,
        subject: SubjectId,
        capability: Capability,
    },
    Revoke {
        object: ObjectId,
        subject: SubjectId,
        capability: Capability,
    },
    Tombstone {
        object: ObjectId,
    },
}

#[derive(Debug, Clone)]
pub struct Transaction {
    id: TransactionId,
    context: AccessContext,
    expected: BTreeMap<ObjectId, ObjectVersion>,
    operations: Vec<Operation>,
}

impl Transaction {
    #[must_use]
    pub fn new(context: AccessContext) -> Self {
        Self {
            id: TransactionId::new(),
            context,
            expected: BTreeMap::new(),
            operations: Vec::new(),
        }
    }

    #[must_use]
    pub const fn id(&self) -> TransactionId {
        self.id
    }

    pub fn expect(&mut self, object: ObjectId, version: ObjectVersion) -> &mut Self {
        self.expected.insert(object, version);
        self
    }

    pub fn create(&mut self, request: CreateObject) -> &mut Self {
        self.operations.push(Operation::Create(request));
        self
    }

    pub fn update_state(&mut self, object: ObjectId, state: impl Into<Vec<u8>>) -> &mut Self {
        self.operations.push(Operation::UpdateState {
            object,
            state: state.into(),
        });
        self
    }

    pub fn set_link(
        &mut self,
        source: ObjectId,
        name: impl Into<String>,
        target: ObjectId,
    ) -> &mut Self {
        self.operations.push(Operation::SetLink {
            source,
            name: name.into(),
            target,
        });
        self
    }

    pub fn remove_link(&mut self, source: ObjectId, name: impl Into<String>) -> &mut Self {
        self.operations.push(Operation::RemoveLink {
            source,
            name: name.into(),
        });
        self
    }

    pub fn reparent(&mut self, child: ObjectId, new_parent: Option<ObjectId>) -> &mut Self {
        self.operations
            .push(Operation::Reparent { child, new_parent });
        self
    }

    pub fn grant(
        &mut self,
        object: ObjectId,
        subject: SubjectId,
        capability: Capability,
    ) -> &mut Self {
        self.operations.push(Operation::Grant {
            object,
            subject,
            capability,
        });
        self
    }

    pub fn revoke(
        &mut self,
        object: ObjectId,
        subject: SubjectId,
        capability: Capability,
    ) -> &mut Self {
        self.operations.push(Operation::Revoke {
            object,
            subject,
            capability,
        });
        self
    }

    pub fn tombstone(&mut self, object: ObjectId) -> &mut Self {
        self.operations.push(Operation::Tombstone { object });
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommitResult {
    pub transaction_id: TransactionId,
    pub versions: BTreeMap<ObjectId, ObjectVersion>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OmsStats {
    pub shard_count: u32,
    pub object_count: usize,
    pub active_count: usize,
    pub tombstoned_count: usize,
}

/// Read-only estimate of reclaimable Tombstone payload and durable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GcAnalysis {
    pub objects_scanned: u64,
    pub active_objects: u64,
    pub tombstones: u64,
    pub tombstones_waiting_for_retention: u64,
    pub objects_compactable: u64,
    pub live_payload_bytes: u64,
    pub dead_payload_bytes: u64,
    pub payload_bytes_reclaimable: u64,
    pub store_bytes_before: u64,
    pub wal_bytes_before: u64,
    pub estimated_store_bytes_after: u64,
}

/// Result of one exclusive, type-agnostic Tombstone compaction pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GcReport {
    pub objects_scanned: u64,
    pub active_objects: u64,
    pub tombstones: u64,
    pub tombstones_waiting_for_retention: u64,
    pub objects_compacted: u64,
    pub live_payload_bytes: u64,
    pub dead_payload_bytes: u64,
    pub payload_bytes_reclaimed: u64,
    pub store_bytes_before: u64,
    pub store_bytes_after: u64,
    pub wal_bytes_before: u64,
    pub wal_bytes_after: u64,
    pub bytes_reclaimed: u64,
    pub gc_duration_millis: u128,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageUsage {
    pub store_bytes: u64,
    pub wal_bytes: u64,
}

/// Internal kernel maintenance service that reclaims payloads from Objects
/// retired for at least seven days. It performs a sweep immediately, then
/// waits for the next expiry while the system runtime is active.
#[derive(Debug)]
pub struct TombstoneReaper {
    shutdown: Sender<()>,
    worker: Option<JoinHandle<()>>,
    manager: std::sync::Weak<InMemoryObjectManager>,
}

impl TombstoneReaper {
    /// Starts the background retirement cleanup service.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the kernel cannot start the worker thread.
    pub fn start(manager: &Arc<InMemoryObjectManager>) -> Result<Self, OmsError> {
        let (shutdown, receiver) = mpsc::channel();
        {
            let mut active = manager
                .reaper_wakeup
                .lock()
                .map_err(|_| OmsError::TemporarilyUnavailable)?;
            if active.is_some() {
                return Err(OmsError::InvalidOperation(
                    "a Tombstone reaper is already running",
                ));
            }
            *active = Some(shutdown.clone());
        }
        let manager_weak = Arc::downgrade(manager);
        let worker_manager = Arc::clone(manager);
        let worker = match thread::Builder::new()
            .name("ousject-tombstone-reaper".to_owned())
            .spawn(move || tombstone_reaper_loop(worker_manager, receiver))
        {
            Ok(worker) => worker,
            Err(error) => {
                if let Some(manager) = manager_weak.upgrade() {
                    if let Ok(mut active) = manager.reaper_wakeup.lock() {
                        active.take();
                    }
                }
                return Err(storage_error(error));
            }
        };
        Ok(Self {
            shutdown,
            worker: Some(worker),
            manager: manager_weak,
        })
    }
}

impl Drop for TombstoneReaper {
    fn drop(&mut self) {
        let _ = self.shutdown.send(());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
        if let Some(manager) = self.manager.upgrade() {
            if let Ok(mut active) = manager.reaper_wakeup.lock() {
                active.take();
            }
        }
    }
}

// These values are deliberately moved into the worker so its lifetime owns
// both the manager lease and the shutdown receiver.
#[allow(clippy::needless_pass_by_value)]
fn tombstone_reaper_loop(manager: Arc<InMemoryObjectManager>, shutdown: Receiver<()>) {
    loop {
        if let Err(error) = manager.reap_expired_tombstones() {
            eprintln!("Ousject tombstone cleanup failed: {error}");
        }
        match manager.tombstone_reaper_wait() {
            None => match shutdown.recv() {
                Ok(()) | Err(_) => return,
            },
            Some(wait) => {
                let wait = if wait.is_zero() {
                    TOMBSTONE_REAPER_RETRY
                } else {
                    wait
                };
                match shutdown.recv_timeout(wait) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => return,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        }
    }
}

pub trait ObjectManager: Send + Sync {
    /// Reads an object using the caller's access context.
    ///
    /// # Errors
    ///
    /// Returns an OMS error if lookup or authorization fails.
    fn read(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError>;

    /// Inspects object metadata using the caller's access context.
    ///
    /// # Errors
    ///
    /// Returns an OMS error if lookup or authorization fails.
    fn inspect(&self, context: AccessContext, object: ObjectId) -> Result<ObjectHeader, OmsError>;

    fn begin(&self, context: AccessContext) -> Transaction;

    /// Atomically commits a transaction.
    ///
    /// # Errors
    ///
    /// Returns an OMS error if validation or publication fails.
    fn commit(&self, transaction: Transaction) -> Result<CommitResult, OmsError>;

    /// Lists objects visible through the `Inspect` capability.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::TemporarilyUnavailable`] if a shard lock is poisoned.
    fn list(&self, context: AccessContext) -> Result<Vec<ObjectHeader>, OmsError>;
}

pub trait SnapshotBackend: Send + Sync + std::fmt::Debug {
    /// Loads the latest durable snapshot, or `None` for an empty store.
    ///
    /// # Errors
    ///
    /// Returns a storage error when the backend cannot read its state.
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError>;

    /// Atomically replaces the durable snapshot.
    ///
    /// # Errors
    ///
    /// Returns a storage error if durability cannot be confirmed.
    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError>;

    /// Forces a durable full checkpoint when the backend supports one.
    ///
    /// # Errors
    ///
    /// Returns a storage error if durability cannot be confirmed.
    fn checkpoint(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        self.store(snapshot)
    }

    /// Returns physical checkpoint and WAL sizes when the backend exposes them.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the backend cannot inspect its durable files.
    fn storage_usage(&self) -> Result<StorageUsage, OmsError> {
        Ok(StorageUsage::default())
    }

    /// Durably switches to a compacted complete image.
    ///
    /// Backends without a specialized generation protocol may use their
    /// ordinary checkpoint operation.
    ///
    /// # Errors
    ///
    /// Returns a storage error if the compacted image cannot be made durable.
    fn compact(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        self.checkpoint(snapshot)
    }
}

#[derive(Debug)]
struct StoreLease {
    path: PathBuf,
    owner: String,
}

impl Drop for StoreLease {
    fn drop(&mut self) {
        if fs::read_to_string(&self.path).is_ok_and(|contents| contents.trim() == self.owner) {
            let _ = fs::remove_file(&self.path);
        }
    }
}

#[derive(Debug, Clone)]
pub struct FileSnapshotBackend {
    path: PathBuf,
    lease: Arc<Mutex<Option<StoreLease>>>,
    commits_since_checkpoint: Arc<AtomicU64>,
    latest: Arc<Mutex<Option<Vec<u8>>>>,
    generation: Arc<AtomicU64>,
    requires_reopen: Arc<AtomicBool>,
}

impl FileSnapshotBackend {
    #[must_use]
    pub fn new(path: impl AsRef<Path>) -> Self {
        Self {
            path: path.as_ref().to_path_buf(),
            lease: Arc::new(Mutex::new(None)),
            commits_since_checkpoint: Arc::new(AtomicU64::new(0)),
            latest: Arc::new(Mutex::new(None)),
            generation: Arc::new(AtomicU64::new(0)),
            requires_reopen: Arc::new(AtomicBool::new(false)),
        }
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn manifest_path(&self) -> PathBuf {
        self.path.with_extension("manifest")
    }

    fn snapshot_path(&self, generation: u64) -> PathBuf {
        generation_path(&self.path, generation, "oms")
    }

    fn wal_path_for(&self, generation: u64) -> PathBuf {
        generation_path(&self.path, generation, "wal")
    }

    fn active_generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }

    fn wal_path(&self) -> PathBuf {
        self.wal_path_for(self.active_generation())
    }

    fn switch_generation(
        &self,
        snapshot: &[u8],
        latest: &mut Option<Vec<u8>>,
    ) -> Result<(), OmsError> {
        let old_generation = self.active_generation();
        let new_generation = old_generation
            .checked_add(1)
            .ok_or_else(|| OmsError::Storage("Object Store generation overflow".to_owned()))?;
        let new_snapshot = self.snapshot_path(new_generation);
        let new_wal = self.wal_path_for(new_generation);

        // A generation newer than the manifest can only be an interrupted,
        // uncommitted attempt. Replace it before preparing the next switch.
        remove_file_if_exists(&new_wal)?;
        persist_file_snapshot(&new_snapshot, snapshot)?;

        let manifest = encode_generation_manifest(new_generation);
        let manifest_directory_synced =
            persist_generation_manifest(&self.manifest_path(), &manifest)?;

        self.publish_generation(new_generation, snapshot, latest);
        if !manifest_directory_synced {
            // A rename is visible, but its directory entry could not be
            // confirmed durable. Either manifest may survive a crash; retain
            // both complete generations so either recovery path is valid.
            self.requires_reopen.store(true, Ordering::Release);
            return Ok(());
        }
        // The new manifest and snapshot are durable. Reclaim older generations
        // only after that switch point; cleanup failures leave harmless orphans.
        for generation in 0..new_generation {
            let _ = remove_file_if_exists(&self.snapshot_path(generation));
            let _ = remove_file_if_exists(&self.wal_path_for(generation));
        }
        let _ = sync_parent_directory(&self.path);
        Ok(())
    }

    fn publish_generation(&self, generation: u64, snapshot: &[u8], latest: &mut Option<Vec<u8>>) {
        self.generation.store(generation, Ordering::Release);
        latest.replace(snapshot.to_vec());
        self.commits_since_checkpoint.store(0, Ordering::Release);
    }

    fn lock_path(&self) -> PathBuf {
        self.path.with_extension("lock")
    }

    fn ensure_lease(&self) -> Result<(), OmsError> {
        let mut lease = self
            .lease
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        if lease.is_some() {
            return Ok(());
        }
        if let Some(parent) = self
            .path
            .parent()
            .filter(|value| !value.as_os_str().is_empty())
        {
            fs::create_dir_all(parent).map_err(storage_error)?;
        }
        let path = self.lock_path();
        let owner = format!("{} {}", std::process::id(), ObjectId::new());
        let mut file = match create_lock_file(&path) {
            Ok(file) => file,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                if stale_process_lock(&path)? {
                    match fs::remove_file(&path) {
                        Ok(()) => create_lock_file(&path).map_err(|retry| {
                            if retry.kind() == std::io::ErrorKind::AlreadyExists {
                                OmsError::StoreInUse(path.display().to_string())
                            } else {
                                storage_error(retry)
                            }
                        })?,
                        Err(remove) if remove.kind() == std::io::ErrorKind::NotFound => {
                            create_lock_file(&path).map_err(storage_error)?
                        }
                        Err(remove) => return Err(storage_error(remove)),
                    }
                } else {
                    return Err(OmsError::StoreInUse(path.display().to_string()));
                }
            }
            Err(error) => return Err(storage_error(error)),
        };
        writeln!(file, "{owner}").map_err(storage_error)?;
        file.sync_all().map_err(storage_error)?;
        *lease = Some(StoreLease { path, owner });
        Ok(())
    }
}

fn create_lock_file(path: &Path) -> Result<File, std::io::Error> {
    OpenOptions::new().create_new(true).write(true).open(path)
}

fn stale_process_lock(path: &Path) -> Result<bool, OmsError> {
    let contents = fs::read_to_string(path).map_err(storage_error)?;
    let pid = contents
        .split_whitespace()
        .next()
        .and_then(|value| value.parse::<u32>().ok());
    let proc_root = Path::new("/proc");
    Ok(proc_root.join("self").exists()
        && pid.is_some_and(|pid| !proc_root.join(pid.to_string()).exists()))
}

impl SnapshotBackend for FileSnapshotBackend {
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError> {
        self.ensure_lease()?;
        let manifest_generation = read_generation_manifest(&self.manifest_path())?;
        let generation = manifest_generation.unwrap_or(0);
        let snapshot_path = self.snapshot_path(generation);
        if manifest_generation.is_some() && !snapshot_path.exists() {
            return Err(corruption(
                "active Object Store generation is missing its checkpoint",
            ));
        }
        let snapshot = if snapshot_path.exists() {
            Some(fs::read(&snapshot_path).map_err(storage_error)?)
        } else {
            None
        };
        let recovered = load_wal(&self.wal_path_for(generation), snapshot)?;
        self.generation.store(generation, Ordering::Release);
        self.latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?
            .clone_from(&recovered);
        let orphan_generation = generation.saturating_add(1);
        for candidate in 0..=orphan_generation {
            let _ = remove_file_if_exists(&temporary_path(&self.snapshot_path(candidate)));
            let _ = remove_file_if_exists(&temporary_path(&self.wal_path_for(candidate)));
        }
        let _ = remove_file_if_exists(&temporary_path(&self.manifest_path()));
        for obsolete in 0..generation {
            let _ = remove_file_if_exists(&self.snapshot_path(obsolete));
            let _ = remove_file_if_exists(&self.wal_path_for(obsolete));
        }
        let _ = remove_file_if_exists(&self.snapshot_path(orphan_generation));
        let _ = remove_file_if_exists(&self.wal_path_for(orphan_generation));
        let _ = sync_parent_directory(&self.path);
        Ok(recovered)
    }

    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        if self.requires_reopen.load(Ordering::Acquire) {
            return Err(OmsError::Storage(
                "generation switch durability is uncertain; reopen the Object Store".to_owned(),
            ));
        }
        self.ensure_lease()?;
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        let payload = encode_wal_payload(latest.as_deref(), snapshot)?;
        append_wal(&self.wal_path(), &payload)?;
        *latest = Some(snapshot.to_vec());

        let pending = self
            .commits_since_checkpoint
            .fetch_add(1, Ordering::Relaxed)
            .saturating_add(1);
        if pending < CHECKPOINT_COMMIT_INTERVAL {
            return Ok(());
        }

        // Checkpoint failure does not undo the already-synced transaction.
        // Switch to a new generation instead of truncating the active WAL.
        if pending >= CHECKPOINT_COMMIT_INTERVAL {
            let _ = self.switch_generation(snapshot, &mut latest);
        }
        Ok(())
    }

    fn checkpoint(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        if self.requires_reopen.load(Ordering::Acquire) {
            return Err(OmsError::Storage(
                "generation switch durability is uncertain; reopen the Object Store".to_owned(),
            ));
        }
        self.ensure_lease()?;
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        // A reset record is a complete independent generation. If power is
        // lost after this flush, recovery can use it with either the old or
        // the newly renamed checkpoint. Only after the new checkpoint and its
        // directory entry are durable may the covered WAL be discarded.
        self.switch_generation(snapshot, &mut latest)
    }

    fn storage_usage(&self) -> Result<StorageUsage, OmsError> {
        self.ensure_lease()?;
        let generation = self.active_generation();
        let mut usage = StorageUsage::default();
        for candidate in 0..=generation.saturating_add(1) {
            usage.store_bytes = usage
                .store_bytes
                .saturating_add(file_size(&self.snapshot_path(candidate))?);
            usage.wal_bytes = usage
                .wal_bytes
                .saturating_add(file_size(&self.wal_path_for(candidate))?);
            usage.store_bytes = usage
                .store_bytes
                .saturating_add(file_size(&temporary_path(&self.snapshot_path(candidate)))?);
            usage.wal_bytes = usage
                .wal_bytes
                .saturating_add(file_size(&temporary_path(&self.wal_path_for(candidate)))?);
        }
        usage.store_bytes = usage
            .store_bytes
            .saturating_add(file_size(&self.manifest_path())?)
            .saturating_add(file_size(&temporary_path(&self.manifest_path()))?);
        Ok(usage)
    }

    fn compact(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        if self.requires_reopen.load(Ordering::Acquire) {
            return Err(OmsError::Storage(
                "generation switch durability is uncertain; reopen the Object Store".to_owned(),
            ));
        }
        self.ensure_lease()?;
        let mut latest = self
            .latest
            .lock()
            .map_err(|_| OmsError::TemporarilyUnavailable)?;
        self.switch_generation(snapshot, &mut latest)
    }
}

fn file_size(path: &Path) -> Result<u64, OmsError> {
    match fs::metadata(path) {
        Ok(metadata) => Ok(metadata.len()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(0),
        Err(error) => Err(storage_error(error)),
    }
}

fn generation_path(path: &Path, generation: u64, extension: &str) -> PathBuf {
    if generation == 0 {
        if extension == "oms" {
            path.to_path_buf()
        } else {
            path.with_extension(extension)
        }
    } else {
        path.with_extension(format!("{extension}.g{generation}"))
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    let mut temporary_name = path.as_os_str().to_os_string();
    temporary_name.push(".tmp");
    PathBuf::from(temporary_name)
}

fn encode_generation_manifest(generation: u64) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(20);
    bytes.extend_from_slice(MANIFEST_MAGIC);
    bytes.extend_from_slice(&generation.to_le_bytes());
    let checksum = wal_checksum(&bytes);
    bytes.extend_from_slice(&checksum.to_le_bytes());
    bytes
}

fn read_generation_manifest(path: &Path) -> Result<Option<u64>, OmsError> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(storage_error(error)),
    };
    if bytes.len() != 20 || &bytes[..4] != MANIFEST_MAGIC {
        return Err(corruption("invalid Object Store generation manifest"));
    }
    let expected = u64::from_le_bytes(
        bytes[12..20]
            .try_into()
            .map_err(|_| corruption("truncated Object Store manifest checksum"))?,
    );
    if wal_checksum(&bytes[..12]) != expected {
        return Err(corruption(
            "Object Store generation manifest checksum mismatch",
        ));
    }
    Ok(Some(u64::from_le_bytes(bytes[4..12].try_into().map_err(
        |_| corruption("truncated Object Store manifest generation"),
    )?)))
}

fn remove_file_if_exists(path: &Path) -> Result<(), OmsError> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(storage_error(error)),
    }
}

fn sync_parent_directory(path: &Path) -> Result<(), OmsError> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        File::open(parent)
            .and_then(|directory| directory.sync_all())
            .map_err(storage_error)?;
    }
    Ok(())
}

#[derive(Debug, Default, Clone)]
struct ShardState {
    objects: BTreeMap<ObjectId, ObjectRecord>,
    by_type: BTreeMap<TypeId, BTreeSet<ObjectId>>,
}

enum LockedShard<'a> {
    Read(RwLockReadGuard<'a, ShardState>),
    Write(RwLockWriteGuard<'a, ShardState>),
}

impl LockedShard<'_> {
    fn state(&self) -> &ShardState {
        match self {
            Self::Read(state) => state,
            Self::Write(state) => state,
        }
    }
}

#[derive(Debug)]
pub struct InMemoryObjectManager {
    directory: FixedDirectory,
    shards: Vec<RwLock<ShardState>>,
    persistence: Option<Arc<dyn SnapshotBackend>>,
    types: TypeRegistry,
    next_tombstone_reap_unix_ms: AtomicU64,
    reaper_wakeup: Mutex<Option<Sender<()>>>,
}

impl InMemoryObjectManager {
    /// Creates an in-memory manager with fixed shard routing.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::InvalidShardCount`] when `shard_count` is zero.
    pub fn new(shard_count: u32) -> Result<Self, OmsError> {
        let directory = FixedDirectory::new(shard_count)?;
        let shards = (0..shard_count)
            .map(|_| RwLock::new(ShardState::default()))
            .collect();
        Ok(Self {
            directory,
            shards,
            persistence: None,
            types: TypeRegistry::builtins(),
            next_tombstone_reap_unix_ms: AtomicU64::new(u64::MAX),
            reaper_wakeup: Mutex::new(None),
        })
    }

    /// Opens the single-Shard durable Object Store used by the system MVP.
    /// Existing state is validated before becoming visible.
    ///
    /// # Errors
    ///
    /// Returns an error when the snapshot cannot be read or is corrupt.
    pub fn open_persistent(path: impl AsRef<Path>) -> Result<Self, OmsError> {
        Self::open_with_backend(Arc::new(FileSnapshotBackend::new(path)))
    }

    /// Opens a durable Object Store with fixed multi-shard routing.
    ///
    /// The durable representation is one globally consistent image; recovery
    /// repartitions Objects using the same stable directory function.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid shard count, an in-use Store, storage
    /// failure, or corrupt durable state.
    pub fn open_persistent_with_shards(
        path: impl AsRef<Path>,
        shard_count: u32,
    ) -> Result<Self, OmsError> {
        Self::open_with_backend_and_shards(Arc::new(FileSnapshotBackend::new(path)), shard_count)
    }

    /// Opens a single-Shard store using a replaceable persistence backend.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot load a valid snapshot.
    pub fn open_with_backend(backend: Arc<dyn SnapshotBackend>) -> Result<Self, OmsError> {
        Self::open_with_backend_and_shards(backend, 1)
    }

    /// Opens a durable Store and repartitions its globally consistent state.
    ///
    /// # Errors
    ///
    /// Returns an error when the backend cannot load a valid snapshot or the
    /// shard count is zero.
    pub fn open_with_backend_and_shards(
        backend: Arc<dyn SnapshotBackend>,
        shard_count: u32,
    ) -> Result<Self, OmsError> {
        let recovered = backend.load()?;
        let needs_retirement_time_upgrade = recovered.as_ref().is_some_and(|snapshot| {
            snapshot.get(4..8) != Some(RETIREMENT_TIME_EXTENSION.as_slice())
        });
        let state = recovered.map_or_else(
            || Ok(ShardState::default()),
            |bytes| decode_snapshot(&bytes),
        )?;
        validate_parent_graph(&state)?;
        validate_type_index(&state)?;
        let types = TypeRegistry::builtins();
        validate_dynamic_types(&state, &types)?;
        if needs_retirement_time_upgrade {
            // Persist upgrade-time timestamps once, so later restarts do not
            // restart the retention period for legacy Tombstones.
            backend.compact(&encode_snapshot(&state)?)?;
        }
        let next_tombstone_reap_unix_ms = next_tombstone_reap_deadline(&state);
        let directory = FixedDirectory::new(shard_count)?;
        let shards = partition_shards(state, &directory)
            .into_iter()
            .map(RwLock::new)
            .collect();
        Ok(Self {
            directory,
            shards,
            persistence: Some(backend),
            types,
            next_tombstone_reap_unix_ms: AtomicU64::new(next_tombstone_reap_unix_ms),
            reaper_wakeup: Mutex::new(None),
        })
    }

    /// Returns every valid built-in and persistent dynamic Type descriptor.
    ///
    /// # Errors
    ///
    /// Returns an error if persistent descriptor state is malformed.
    pub fn types(&self) -> Result<Vec<TypeDescriptor>, OmsError> {
        let mut types = self.types.all();
        types.extend(self.dynamic_types()?);
        types.sort_by_key(|descriptor| descriptor.id);
        Ok(types)
    }

    /// Resolves a registered type by its stable source name.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::UnknownTypeName`] when no descriptor is registered.
    pub fn type_by_name(&self, name: &str) -> Result<TypeDescriptor, OmsError> {
        match self.types.by_name(name) {
            Ok(descriptor) => Ok(descriptor.clone()),
            Err(OmsError::UnknownTypeName(_)) => self
                .dynamic_types()?
                .into_iter()
                .find(|descriptor| descriptor.name == name)
                .ok_or_else(|| OmsError::UnknownTypeName(name.to_owned())),
            Err(error) => Err(error),
        }
    }

    /// Resolves a registered type by its stable identifier.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::UnknownType`] when no descriptor is registered.
    pub fn type_by_id(&self, id: TypeId) -> Result<TypeDescriptor, OmsError> {
        match self.types.by_id(id) {
            Ok(descriptor) => Ok(descriptor.clone()),
            Err(OmsError::UnknownType(_)) => self
                .dynamic_types()?
                .into_iter()
                .find(|descriptor| descriptor.id == id)
                .ok_or(OmsError::UnknownType(id)),
            Err(error) => Err(error),
        }
    }

    /// Registers a persistent Type Descriptor Object at runtime.
    ///
    /// # Errors
    ///
    /// Returns an error unless called by the trusted system identity, or when
    /// the name/id already exists or persistence fails.
    pub fn register_type(
        &self,
        context: AccessContext,
        name: &str,
        schema: ValueSchema,
        creation: CreationPolicy,
        domain_capabilities: BTreeSet<String>,
    ) -> Result<TypeDescriptor, OmsError> {
        let (descriptor, request) =
            self.prepare_register_type(context, name, schema, creation, domain_capabilities)?;
        let mut transaction = self.begin(context);
        transaction.create(request);
        self.commit(transaction)?;
        Ok(descriptor)
    }

    /// Validates a dynamic Type registration without committing it, allowing
    /// the descriptor to join a larger atomic transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for an untrusted caller, invalid/duplicate name, or
    /// an encoding failure.
    pub fn prepare_register_type(
        &self,
        context: AccessContext,
        name: &str,
        schema: ValueSchema,
        creation: CreationPolicy,
        domain_capabilities: BTreeSet<String>,
    ) -> Result<(TypeDescriptor, CreateObject), OmsError> {
        if context.subject != SYSTEM_SUBJECT {
            return Err(OmsError::InvalidOperation(
                "only the system subject can register Types",
            ));
        }
        if name.is_empty() || name.contains('\0') || self.type_by_name(name).is_ok() {
            return Err(OmsError::InvalidOperation(
                "Type name is invalid or already exists",
            ));
        }
        let descriptor = TypeDescriptor {
            id: TypeId::new(),
            name: name.to_owned(),
            schema,
            creation,
            capabilities: all_capabilities(),
            domain_capabilities,
        };
        let state = encode_type_descriptor(&descriptor)?;
        Ok((descriptor, CreateObject::new(TYPE_DESCRIPTOR_TYPE, state)))
    }

    fn dynamic_types(&self) -> Result<Vec<TypeDescriptor>, OmsError> {
        let mut descriptors = Vec::new();
        for shard in &self.shards {
            let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
            if let Some(objects) = state.by_type.get(&TYPE_DESCRIPTOR_TYPE) {
                for object in objects {
                    let record = state
                        .objects
                        .get(object)
                        .ok_or(OmsError::NotFound(*object))?;
                    if record.header.lifecycle != LifecycleState::Tombstoned {
                        descriptors.push(decode_type_descriptor(&record.state)?);
                    }
                }
            }
        }
        Ok(descriptors)
    }

    /// Validates a public typed creation without committing it. The caller may
    /// stage the returned request alongside other changes in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or provider-only type, a schema mismatch
    /// or an invalid Value.
    pub fn prepare_create(&self, spec: CreateSpec) -> Result<CreateObject, OmsError> {
        let descriptor = self.type_by_name(&spec.type_name)?;
        if descriptor.creation != CreationPolicy::Public {
            return Err(OmsError::TypeCreationDenied(descriptor.id));
        }
        let value = normalize_value(descriptor.schema, &spec.value);
        if !descriptor.schema.accepts(&value) {
            return Err(OmsError::ValueSchemaMismatch {
                type_id: descriptor.id,
                expected: descriptor.schema.name(),
                actual: value.kind(),
            });
        }
        let encoded = value
            .encode()
            .map_err(|error| OmsError::InvalidValue(error.to_string()))?;
        let mut request = CreateObject::new(descriptor.id, encoded);
        request.parent = spec.parent;
        request.links = spec.links;
        request.capabilities.clone_from(&descriptor.capabilities);
        Ok(request)
    }

    /// Creates a typed Value Object using capabilities supplied by its type.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown or provider-only type, a schema mismatch,
    /// an invalid Value, or a failed atomic commit.
    pub fn create_object(
        &self,
        context: AccessContext,
        spec: CreateSpec,
    ) -> Result<ObjectId, OmsError> {
        let request = self.prepare_create(spec)?;
        let id = request.id;
        let mut transaction = self.begin(context);
        if let Some(parent) = request.parent {
            let version = self.inspect(context, parent)?.version;
            transaction.expect(parent, version);
        }
        transaction.create(request);
        self.commit(transaction)?;
        Ok(id)
    }

    /// Finds and returns the current immutable Object view.
    ///
    /// # Errors
    ///
    /// Returns an error when lookup or `ViewValue` authorization fails.
    pub fn find(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError> {
        self.read(context, object)
    }

    /// Decodes the current Object Value.
    ///
    /// # Errors
    ///
    /// Returns an error for denied access or a non-Value legacy state.
    pub fn value(&self, context: AccessContext, object: ObjectId) -> Result<Value, OmsError> {
        let view = self.read(context, object)?;
        Value::decode(view.state()).map_err(|error| OmsError::InvalidValue(error.to_string()))
    }

    /// Atomically replaces a typed Object Value using optimistic versioning.
    ///
    /// # Errors
    ///
    /// Returns an error for type mismatch, denied access or commit failure.
    pub fn replace_value(
        &self,
        context: AccessContext,
        object: ObjectId,
        value: &Value,
    ) -> Result<ObjectVersion, OmsError> {
        let (version, encoded) = self.prepare_replace_value(context, object, value)?;
        let mut transaction = self.begin(context);
        transaction
            .expect(object, version)
            .update_state(object, encoded);
        let result = self.commit(transaction)?;
        result
            .versions
            .get(&object)
            .copied()
            .ok_or(OmsError::InvalidOperation("commit omitted replaced object"))
    }

    /// Validates a public Value replacement without committing it, so callers
    /// can stage it with Process state in one transaction.
    ///
    /// # Errors
    ///
    /// Returns an error for denied access, a provider-owned type, a schema
    /// mismatch, or an invalid Value.
    pub fn prepare_replace_value(
        &self,
        context: AccessContext,
        object: ObjectId,
        value: &Value,
    ) -> Result<(ObjectVersion, Vec<u8>), OmsError> {
        let view = self.read(context, object)?;
        let descriptor = self.type_by_id(view.header().type_id)?;
        if descriptor.creation != CreationPolicy::Public {
            return Err(OmsError::TypeCreationDenied(descriptor.id));
        }
        let normalized = normalize_value(descriptor.schema, value);
        if !descriptor.schema.accepts(&normalized) {
            return Err(OmsError::ValueSchemaMismatch {
                type_id: descriptor.id,
                expected: descriptor.schema.name(),
                actual: normalized.kind(),
            });
        }
        let encoded = normalized
            .encode()
            .map_err(|error| OmsError::InvalidValue(error.to_string()))?;
        Ok((view.header().version, encoded))
    }

    /// Queries visible Objects using indexed type lookup when available.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::TemporarilyUnavailable`] if a shard lock is poisoned.
    pub fn query(
        &self,
        context: AccessContext,
        query: &ObjectQuery,
    ) -> Result<Vec<ObjectHeader>, OmsError> {
        let mut matches = Vec::new();
        let descriptors = self.types()?;
        for shard in &self.shards {
            let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
            let candidates: Box<dyn Iterator<Item = &ObjectRecord> + '_> =
                if let Some(type_id) = query.type_id {
                    Box::new(
                        state
                            .by_type
                            .get(&type_id)
                            .into_iter()
                            .flatten()
                            .filter_map(|id| state.objects.get(id)),
                    )
                } else {
                    Box::new(state.objects.values())
                };
            for record in candidates {
                if record.header.lifecycle == LifecycleState::Tombstoned
                    || query
                        .parent
                        .is_some_and(|parent| record.header.parent_id != Some(parent))
                    || !record.policy.allows(context.subject, Capability::Inspect)
                    || !record.capabilities.contains(&Capability::Inspect)
                    || query.capability.is_some_and(|capability| {
                        !record.capabilities.contains(&capability)
                            || !record.policy.allows(context.subject, capability)
                    })
                {
                    continue;
                }
                if let Some(capability) = &query.domain_capability {
                    let descriptor = descriptors
                        .iter()
                        .find(|descriptor| descriptor.id == record.header.type_id)
                        .ok_or(OmsError::UnknownType(record.header.type_id))?;
                    if !descriptor.domain_capabilities.contains(capability)
                        || !record.capabilities.contains(&Capability::Invoke)
                        || !record.policy.allows(context.subject, Capability::Invoke)
                    {
                        continue;
                    }
                }
                matches.push(record.header.clone());
            }
        }
        matches.sort_by_key(|header| header.id);
        Ok(matches)
    }

    /// Atomically binds a name in a Namespace Object to any Object.
    ///
    /// # Errors
    ///
    /// Returns an error for an invalid or duplicate name, wrong Object type,
    /// denied access, missing target or failed commit.
    pub fn bind_name(
        &self,
        context: AccessContext,
        namespace: ObjectId,
        name: &str,
        target: ObjectId,
    ) -> Result<ObjectVersion, OmsError> {
        validate_name(name)?;
        let view = self.read(context, namespace)?;
        if view.header().type_id != CORE_NAMESPACE_TYPE {
            return Err(OmsError::InvalidOperation(
                "names can only be bound in a core.namespace Object",
            ));
        }
        if view.links().contains_key(name) {
            return Err(OmsError::InvalidOperation("namespace name already exists"));
        }
        let mut transaction = self.begin(context);
        transaction
            .expect(namespace, view.header().version)
            .set_link(namespace, name, target);
        let result = self.commit(transaction)?;
        committed_version(&result, namespace)
    }

    /// Atomically removes a name from a Namespace Object.
    ///
    /// # Errors
    ///
    /// Returns an error for a missing name, wrong Object type, denied access or
    /// failed commit.
    pub fn unbind_name(
        &self,
        context: AccessContext,
        namespace: ObjectId,
        name: &str,
    ) -> Result<ObjectVersion, OmsError> {
        validate_name(name)?;
        let view = self.read(context, namespace)?;
        if view.header().type_id != CORE_NAMESPACE_TYPE {
            return Err(OmsError::InvalidOperation(
                "names can only be removed from a core.namespace Object",
            ));
        }
        if !view.links().contains_key(name) {
            return Err(OmsError::NameNotFound {
                namespace,
                name: name.to_owned(),
            });
        }
        let mut transaction = self.begin(context);
        transaction
            .expect(namespace, view.header().version)
            .remove_link(namespace, name);
        let result = self.commit(transaction)?;
        committed_version(&result, namespace)
    }

    /// Resolves a slash-separated Namespace path to a stable `ObjectId`.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid components, missing names, non-Namespace
    /// intermediate Objects or denied visibility.
    pub fn resolve(
        &self,
        context: AccessContext,
        root: ObjectId,
        path: &str,
    ) -> Result<ObjectId, OmsError> {
        let trimmed = path.trim_matches('/');
        if trimmed.is_empty() {
            self.inspect(context, root)?;
            return Ok(root);
        }
        let mut current = root;
        for name in trimmed.split('/') {
            validate_name(name)?;
            let view = self.read(context, current)?;
            if view.header().type_id != CORE_NAMESPACE_TYPE {
                return Err(OmsError::InvalidOperation(
                    "path component is not a core.namespace Object",
                ));
            }
            current = view
                .links()
                .get(name)
                .copied()
                .ok_or_else(|| OmsError::NameNotFound {
                    namespace: current,
                    name: name.to_owned(),
                })?;
        }
        self.inspect(context, current)?;
        Ok(current)
    }

    #[must_use]
    pub fn shard_for(&self, object: ObjectId) -> ShardId {
        self.directory.locate(object)
    }

    /// Returns an immutable view of the latest published object version.
    ///
    /// # Errors
    ///
    /// Returns an error when the object is missing, unavailable, tombstoned,
    /// or the caller lacks the `ViewValue` capability.
    pub fn read(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError> {
        let shard = self.shard(object);
        let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        record.require(context, Capability::ViewValue)?;
        Ok(record.view())
    }

    /// Checks that the caller may use a specific Object capability.
    ///
    /// # Errors
    ///
    /// Returns an error if the Object is absent, unavailable, inactive or denied.
    pub fn require_capability(
        &self,
        context: AccessContext,
        object: ObjectId,
        capability: Capability,
    ) -> Result<(), OmsError> {
        let shard = self.shard(object);
        let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        record.require(context, capability)
    }

    /// Returns object metadata after checking the `Inspect` capability.
    ///
    /// # Errors
    ///
    /// Returns an error when the object is missing, unavailable, tombstoned,
    /// or the caller lacks the `Inspect` capability.
    pub fn inspect(
        &self,
        context: AccessContext,
        object: ObjectId,
    ) -> Result<ObjectHeader, OmsError> {
        let shard = self.shard(object);
        let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
        let record = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?;
        record.require(context, Capability::Inspect)?;
        Ok(record.header.clone())
    }

    /// Lists object headers for which the caller has the `Inspect` capability.
    /// Tombstoned objects remain inspectable but cannot be read or modified.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::TemporarilyUnavailable`] if a shard lock is poisoned.
    pub fn list(&self, context: AccessContext) -> Result<Vec<ObjectHeader>, OmsError> {
        let mut objects = Vec::new();
        for shard in &self.shards {
            let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
            objects.extend(
                state
                    .objects
                    .values()
                    .filter(|record| {
                        record.capabilities.contains(&Capability::Inspect)
                            && record.policy.allows(context.subject, Capability::Inspect)
                    })
                    .map(|record| record.header.clone()),
            );
        }
        objects.sort_by_key(|header| header.id);
        Ok(objects)
    }

    /// Returns aggregate in-memory object counts.
    ///
    /// # Errors
    ///
    /// Returns [`OmsError::TemporarilyUnavailable`] if a shard lock is poisoned.
    pub fn stats(&self) -> Result<OmsStats, OmsError> {
        let mut object_count = 0;
        let mut tombstoned_count = 0;
        for shard in &self.shards {
            let state = shard.read().map_err(|_| OmsError::TemporarilyUnavailable)?;
            object_count += state.objects.len();
            tombstoned_count += state
                .objects
                .values()
                .filter(|record| record.header.lifecycle == LifecycleState::Tombstoned)
                .count();
        }
        Ok(OmsStats {
            shard_count: self.directory.shard_count(),
            object_count,
            active_count: object_count - tombstoned_count,
            tombstoned_count,
        })
    }

    /// Analyzes every Object without modifying memory, WAL or checkpoints.
    /// Only Tombstones at least seven days old are reclaimable.
    ///
    /// # Errors
    ///
    /// Returns an error if shard state is unavailable, inconsistent or cannot
    /// be encoded, or if backend size metadata cannot be read.
    pub fn analyze_gc(&self) -> Result<GcAnalysis, OmsError> {
        self.analyze_gc_at(SystemTime::now())
    }

    /// Estimates cleanup as of `now`; the supplied time is primarily useful to
    /// the kernel scheduler and deterministic retention-policy tests.
    ///
    /// # Errors
    ///
    /// Returns an error if state, time conversion, or storage metadata is
    /// unavailable.
    pub fn analyze_gc_at(&self, now: SystemTime) -> Result<GcAnalysis, OmsError> {
        let cutoff_unix_ms = retention_cutoff_unix_ms(system_time_to_unix_millis(now)?);
        let shards = self
            .shards
            .iter()
            .map(|shard| shard.read().map_err(|_| OmsError::TemporarilyUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        let current = combine_shards(shards.iter().map(|shard| &**shard))?;
        let mut compacted = current.clone();
        let counts = compact_tombstone_payloads(&mut compacted, cutoff_unix_ms);
        validate_gc_candidate(&compacted, &self.types)?;
        let usage = self
            .persistence
            .as_ref()
            .map_or(Ok(StorageUsage::default()), |backend| {
                backend.storage_usage()
            })?;
        Ok(GcAnalysis {
            objects_scanned: counts.objects_scanned,
            active_objects: counts.active_objects,
            tombstones: counts.tombstones,
            tombstones_waiting_for_retention: counts.tombstones_waiting_for_retention,
            objects_compactable: counts.objects_compacted,
            live_payload_bytes: counts.live_payload_bytes,
            dead_payload_bytes: counts.dead_payload_bytes_before,
            payload_bytes_reclaimable: counts
                .dead_payload_bytes_before
                .saturating_sub(counts.dead_payload_bytes_after),
            store_bytes_before: usage.store_bytes,
            wal_bytes_before: usage.wal_bytes,
            estimated_store_bytes_after: u64::try_from(encode_snapshot(&compacted)?.len())
                .map_err(|_| OmsError::Storage("compacted snapshot is too large".to_owned()))?,
        })
    }

    /// Exclusively compacts every eligible Tombstone to its minimal form.
    ///
    /// This is a stop-the-world maintenance operation for this manager. All
    /// shard write locks remain held until the compacted image is durable and
    /// every shard publishes the same candidate. Active Objects are copied
    /// byte-for-byte and `ObjectId`s are never removed or reused.
    ///
    /// # Errors
    ///
    /// Returns an error if locking, validation, encoding or durable generation
    /// switching fails. Before the durable switch, the old state remains live.
    pub fn compact(&self) -> Result<GcReport, OmsError> {
        self.compact_expired_tombstones_at(SystemTime::now())
    }

    /// Compacts Tombstones that have reached the seven-day retention age as
    /// of `now`. The kernel maintenance worker runs this automatically.
    ///
    /// # Errors
    ///
    /// Returns an error if locking, validation, encoding or durable generation
    /// switching fails. Before the durable switch, the old state remains live.
    pub fn compact_expired_tombstones_at(&self, now: SystemTime) -> Result<GcReport, OmsError> {
        let cutoff_unix_ms = retention_cutoff_unix_ms(system_time_to_unix_millis(now)?);
        let started = Instant::now();
        let mut locked = self
            .shards
            .iter()
            .map(|shard| shard.write().map_err(|_| OmsError::TemporarilyUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        let current = combine_shards(locked.iter().map(|shard| &**shard))?;
        let mut candidate = current.clone();
        let counts = compact_tombstone_payloads(&mut candidate, cutoff_unix_ms);
        let next_reap_deadline = next_tombstone_reap_deadline(&candidate);
        validate_gc_candidate(&candidate, &self.types)?;
        let usage_before = self
            .persistence
            .as_ref()
            .map_or(Ok(StorageUsage::default()), |backend| {
                backend.storage_usage()
            })?;

        if counts.objects_compacted != 0 {
            let snapshot = encode_snapshot(&candidate)?;
            if let Some(backend) = &self.persistence {
                backend.compact(&snapshot)?;
            }
            let partitioned = partition_shards(candidate, &self.directory);
            for (target, state) in locked.iter_mut().zip(partitioned) {
                **target = state;
            }
        }

        self.next_tombstone_reap_unix_ms
            .store(next_reap_deadline, Ordering::Release);

        let usage_after = self
            .persistence
            .as_ref()
            .map_or(Ok(StorageUsage::default()), |backend| {
                backend.storage_usage()
            })?;
        let before_total = usage_before
            .store_bytes
            .saturating_add(usage_before.wal_bytes);
        let after_total = usage_after
            .store_bytes
            .saturating_add(usage_after.wal_bytes);
        Ok(GcReport {
            objects_scanned: counts.objects_scanned,
            active_objects: counts.active_objects,
            tombstones: counts.tombstones,
            tombstones_waiting_for_retention: counts.tombstones_waiting_for_retention,
            objects_compacted: counts.objects_compacted,
            live_payload_bytes: counts.live_payload_bytes,
            dead_payload_bytes: counts.dead_payload_bytes_before,
            payload_bytes_reclaimed: counts
                .dead_payload_bytes_before
                .saturating_sub(counts.dead_payload_bytes_after),
            store_bytes_before: usage_before.store_bytes,
            store_bytes_after: usage_after.store_bytes,
            wal_bytes_before: usage_before.wal_bytes,
            wal_bytes_after: usage_after.wal_bytes,
            bytes_reclaimed: before_total.saturating_sub(after_total),
            gc_duration_millis: started.elapsed().as_millis(),
        })
    }

    /// Performs one kernel cleanup sweep using the current wall-clock time.
    ///
    /// # Errors
    ///
    /// Returns an error if the system clock, store, or durable backend fails.
    pub fn reap_expired_tombstones(&self) -> Result<GcReport, OmsError> {
        let now_unix_ms = unix_time_millis()?;
        if self.next_tombstone_reap_unix_ms.load(Ordering::Acquire) > now_unix_ms {
            return Ok(GcReport::default());
        }
        self.compact_expired_tombstones_at(SystemTime::now())
    }

    /// Validates all first-stage in-memory invariants.
    ///
    /// # Errors
    ///
    /// Returns an OMS error when a shard lock is poisoned or an invariant is broken.
    pub fn health_check(&self) -> Result<(), OmsError> {
        let shards = self
            .shards
            .iter()
            .map(|shard| shard.read().map_err(|_| OmsError::TemporarilyUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        let combined = combine_shards(shards.iter().map(|shard| &**shard))?;
        validate_parent_graph(&combined)?;
        validate_type_index(&combined)?;
        validate_dynamic_types(&combined, &self.types)?;
        Ok(())
    }

    /// Writes a verified durable checkpoint of the current committed state.
    /// In-memory stores have nothing to checkpoint and return successfully.
    ///
    /// # Errors
    ///
    /// Returns an error when shard state cannot be read/encoded or the
    /// persistence backend cannot durably replace its checkpoint.
    pub fn checkpoint(&self) -> Result<(), OmsError> {
        let Some(backend) = &self.persistence else {
            return Ok(());
        };
        let shards = self
            .shards
            .iter()
            .map(|shard| shard.read().map_err(|_| OmsError::TemporarilyUnavailable))
            .collect::<Result<Vec<_>, _>>()?;
        let combined = combine_shards(shards.iter().map(|shard| &**shard))?;
        backend.checkpoint(&encode_snapshot(&combined)?)
    }

    #[must_use]
    pub fn begin(&self, context: AccessContext) -> Transaction {
        Transaction::new(context)
    }

    /// Atomically publishes a validated transaction across all affected shards.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty transaction, stale object versions,
    /// invalid relationships or lifecycle changes, denied capabilities,
    /// missing objects, persistence failure, or an unavailable shard lock.
    pub fn commit(&self, transaction: Transaction) -> Result<CommitResult, OmsError> {
        self.commit_batch(vec![transaction])?
            .pop()
            .ok_or(OmsError::InvalidOperation("transaction batch is empty"))
    }

    /// Validates and publishes several transactions with one durable backend
    /// write/flush. Transactions are applied in input order; if any one fails,
    /// none of the batch becomes visible or durable.
    ///
    /// # Errors
    ///
    /// Returns an error for an empty batch/transaction, conflicts, invalid
    /// operations, denied access, persistence failure, or poisoned locks.
    pub fn commit_batch(
        &self,
        transactions: Vec<Transaction>,
    ) -> Result<Vec<CommitResult>, OmsError> {
        if transactions.is_empty() {
            return Err(OmsError::InvalidOperation("transaction batch is empty"));
        }
        if transactions
            .iter()
            .any(|transaction| transaction.operations.is_empty())
        {
            return Err(OmsError::InvalidOperation(
                "transaction contains no operations",
            ));
        }

        let write_shards =
            transaction_write_shards(&transactions, &self.directory, self.shards.len());
        // Every shard remains locked in stable directory order so validation
        // and snapshot encoding see one global state. Only participating
        // shards take exclusive locks; all others use shared read locks.
        let mut locked = self
            .shards
            .iter()
            .enumerate()
            .map(|(index, shard)| {
                if write_shards.contains(&index) {
                    shard
                        .write()
                        .map(LockedShard::Write)
                        .map_err(|_| OmsError::TemporarilyUnavailable)
                } else {
                    shard
                        .read()
                        .map(LockedShard::Read)
                        .map_err(|_| OmsError::TemporarilyUnavailable)
                }
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut candidate = combine_shards(locked.iter().map(LockedShard::state))?;

        let mut results = Vec::with_capacity(transactions.len());
        let mut new_tombstone_deadlines = Vec::new();
        for transaction in transactions {
            validate_expected(&candidate, &transaction.expected)?;
            let mut changed = BTreeSet::new();
            let mut created = BTreeSet::new();

            for operation in transaction.operations {
                apply_operation(
                    &mut candidate,
                    transaction.context,
                    operation,
                    &transaction.expected,
                    &mut changed,
                    &mut created,
                )?;
            }

            validate_parent_graph(&candidate)?;
            validate_type_index(&candidate)?;
            validate_dynamic_types(&candidate, &self.types)?;

            let mut versions = BTreeMap::new();
            for object in changed {
                let record = candidate
                    .objects
                    .get_mut(&object)
                    .ok_or(OmsError::NotFound(object))?;
                if let Some(deadline) = tombstone_reap_deadline(record) {
                    new_tombstone_deadlines.push(deadline);
                }
                if !created.contains(&object) {
                    record.header.version = record
                        .header
                        .version
                        .checked_next()
                        .ok_or(OmsError::VersionExhausted(object))?;
                }
                versions.insert(object, record.header.version);
            }
            results.push(CommitResult {
                transaction_id: transaction.id,
                versions,
            });
        }

        if let Some(backend) = &self.persistence {
            backend.store(&encode_snapshot(&candidate)?)?;
        }

        let partitioned = partition_shards(candidate, &self.directory);
        for (target, state) in locked.iter_mut().zip(partitioned) {
            if let LockedShard::Write(target) = target {
                **target = state;
            }
        }
        for deadline in &new_tombstone_deadlines {
            self.next_tombstone_reap_unix_ms
                .fetch_min(*deadline, Ordering::AcqRel);
        }
        if !new_tombstone_deadlines.is_empty() {
            self.wake_tombstone_reaper();
        }
        // All write guards remain held until every shard contains its new
        // state, so readers see either the old global state or the new one.

        Ok(results)
    }

    fn shard(&self, object: ObjectId) -> &RwLock<ShardState> {
        &self.shards[self.directory.locate(object).get() as usize]
    }

    fn tombstone_reaper_wait(&self) -> Option<Duration> {
        let deadline = self.next_tombstone_reap_unix_ms.load(Ordering::Acquire);
        if deadline == u64::MAX {
            return None;
        }
        let Ok(now) = unix_time_millis() else {
            return Some(TOMBSTONE_REAPER_RETRY);
        };
        Some(Duration::from_millis(deadline.saturating_sub(now)))
    }

    fn wake_tombstone_reaper(&self) {
        if let Ok(active) = self.reaper_wakeup.lock() {
            if let Some(wakeup) = active.as_ref() {
                let _ = wakeup.send(());
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct GcCounts {
    objects_scanned: u64,
    active_objects: u64,
    tombstones: u64,
    tombstones_waiting_for_retention: u64,
    objects_compacted: u64,
    live_payload_bytes: u64,
    dead_payload_bytes_before: u64,
    dead_payload_bytes_after: u64,
}

fn needs_tombstone_compaction(record: &ObjectRecord) -> bool {
    record.header.lifecycle == LifecycleState::Tombstoned
        && !unresolved_effect_state(record.header.type_id, &record.state)
        && (!record.state.is_empty()
            || !record.links.is_empty()
            || record.capabilities != BTreeSet::from([Capability::Inspect])
            || record
                .policy
                .grants
                .values()
                .any(|capabilities| capabilities != &BTreeSet::from([Capability::Inspect])))
}

fn tombstone_reap_deadline(record: &ObjectRecord) -> Option<u64> {
    if !needs_tombstone_compaction(record) {
        return None;
    }
    record
        .retired_at_unix_ms
        .map(|retired_at| retired_at.saturating_add(TOMBSTONE_RETENTION_MILLIS))
}

fn next_tombstone_reap_deadline(state: &ShardState) -> u64 {
    state
        .objects
        .values()
        .filter_map(tombstone_reap_deadline)
        .min()
        .unwrap_or(u64::MAX)
}

fn compact_tombstone_payloads(state: &mut ShardState, cutoff_unix_ms: u64) -> GcCounts {
    let mut counts = GcCounts::default();
    for record in state.objects.values_mut() {
        counts.objects_scanned = counts.objects_scanned.saturating_add(1);
        let payload = u64::try_from(record.state.len()).unwrap_or(u64::MAX);
        if record.header.lifecycle != LifecycleState::Tombstoned {
            counts.active_objects = counts.active_objects.saturating_add(1);
            counts.live_payload_bytes = counts.live_payload_bytes.saturating_add(payload);
            continue;
        }
        counts.tombstones = counts.tombstones.saturating_add(1);
        counts.dead_payload_bytes_before = counts.dead_payload_bytes_before.saturating_add(payload);
        if record
            .retired_at_unix_ms
            .is_none_or(|retired_at| retired_at > cutoff_unix_ms)
        {
            counts.tombstones_waiting_for_retention =
                counts.tombstones_waiting_for_retention.saturating_add(1);
            counts.dead_payload_bytes_after =
                counts.dead_payload_bytes_after.saturating_add(payload);
            continue;
        }
        if unresolved_effect_state(record.header.type_id, &record.state) {
            // An Effect may be in flight or have an unknown external outcome.
            // Keep its full durable idempotency record until it is resolved.
            counts.dead_payload_bytes_after =
                counts.dead_payload_bytes_after.saturating_add(payload);
            continue;
        }
        let compactable = needs_tombstone_compaction(record);
        if compactable {
            counts.objects_compacted = counts.objects_compacted.saturating_add(1);
            record.state = Arc::from(Vec::<u8>::new());
            record.links.clear();
            record.capabilities = BTreeSet::from([Capability::Inspect]);
            record.policy.grants.retain(|_, capabilities| {
                capabilities.retain(|capability| *capability == Capability::Inspect);
                !capabilities.is_empty()
            });
        }
        counts.dead_payload_bytes_after = counts
            .dead_payload_bytes_after
            .saturating_add(u64::try_from(record.state.len()).unwrap_or(u64::MAX));
    }
    counts
}

fn validate_gc_candidate(state: &ShardState, types: &TypeRegistry) -> Result<(), OmsError> {
    validate_parent_graph(state)?;
    validate_type_index(state)?;
    validate_dynamic_types(state, types)
}

fn unresolved_effect_state(type_id: TypeId, state: &[u8]) -> bool {
    if type_id != CORE_EFFECT_TYPE {
        return false;
    }
    let Ok(Value::Record(fields)) = Value::decode(state) else {
        // A malformed Effect cannot safely be classified as resolved.
        return true;
    };
    !matches!(
        fields.get("status"),
        Some(Value::Text(status)) if status == "completed" || status == "failed"
    )
}

fn unix_time_millis() -> Result<u64, OmsError> {
    system_time_to_unix_millis(SystemTime::now())
}

fn system_time_to_unix_millis(time: SystemTime) -> Result<u64, OmsError> {
    let duration = time
        .duration_since(UNIX_EPOCH)
        .map_err(|_| OmsError::Storage("system time is before the Unix epoch".to_owned()))?;
    u64::try_from(duration.as_millis())
        .map_err(|_| OmsError::Storage("Unix time is out of supported range".to_owned()))
}

const fn retention_cutoff_unix_ms(now_unix_ms: u64) -> u64 {
    now_unix_ms.saturating_sub(TOMBSTONE_RETENTION_MILLIS)
}

impl ObjectManager for InMemoryObjectManager {
    fn read(&self, context: AccessContext, object: ObjectId) -> Result<ObjectView, OmsError> {
        Self::read(self, context, object)
    }

    fn inspect(&self, context: AccessContext, object: ObjectId) -> Result<ObjectHeader, OmsError> {
        Self::inspect(self, context, object)
    }

    fn begin(&self, context: AccessContext) -> Transaction {
        Self::begin(self, context)
    }

    fn commit(&self, transaction: Transaction) -> Result<CommitResult, OmsError> {
        Self::commit(self, transaction)
    }

    fn list(&self, context: AccessContext) -> Result<Vec<ObjectHeader>, OmsError> {
        Self::list(self, context)
    }
}

fn transaction_write_shards(
    transactions: &[Transaction],
    directory: &FixedDirectory,
    shard_count: usize,
) -> BTreeSet<usize> {
    let mut shards = BTreeSet::new();
    for transaction in transactions {
        for operation in &transaction.operations {
            let mut add = |object: ObjectId| {
                shards.insert(directory.locate(object).get() as usize);
            };
            match operation {
                Operation::Create(request) => {
                    add(request.id);
                    if let Some(parent) = request.parent {
                        add(parent);
                    }
                }
                Operation::UpdateState { object, .. }
                | Operation::Grant { object, .. }
                | Operation::Revoke { object, .. } => add(*object),
                Operation::SetLink { source, .. } | Operation::RemoveLink { source, .. } => {
                    add(*source);
                }
                // The old parent is stored in current state and is also
                // modified. Tombstoning may likewise detach a parent. Until
                // lock planning reads that metadata without an upgrade race,
                // these uncommon operations conservatively write-lock all.
                Operation::Reparent { .. } | Operation::Tombstone { .. } => {
                    return (0..shard_count).collect();
                }
            }
        }
    }
    shards
}

fn validate_expected(
    state: &ShardState,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
) -> Result<(), OmsError> {
    for (&object, &version) in expected {
        let actual = state
            .objects
            .get(&object)
            .ok_or(OmsError::NotFound(object))?
            .header
            .version;
        if version != actual {
            return Err(OmsError::Conflict {
                object,
                expected: version,
                actual,
            });
        }
    }
    Ok(())
}

fn require_expected(
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    object: ObjectId,
) -> Result<(), OmsError> {
    if expected.contains_key(&object) {
        Ok(())
    } else {
        Err(OmsError::InvalidOperation(
            "every modified existing object needs an expected version",
        ))
    }
}

fn apply_operation(
    state: &mut ShardState,
    context: AccessContext,
    operation: Operation,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
    created: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    match operation {
        Operation::Create(request) => {
            apply_create(state, context, request, expected, changed, created)
        }
        Operation::UpdateState {
            object,
            state: data,
        } => apply_update(state, context, object, data, expected, changed),
        Operation::SetLink {
            source,
            name,
            target,
        } => apply_set_link(state, context, source, name, target, expected, changed),
        Operation::RemoveLink { source, name } => {
            apply_remove_link(state, context, source, &name, expected, changed)
        }
        Operation::Reparent { child, new_parent } => {
            apply_reparent(state, context, child, new_parent, expected, changed)
        }
        Operation::Grant {
            object,
            subject,
            capability,
        } => apply_grant(
            state, context, object, subject, capability, expected, changed,
        ),
        Operation::Revoke {
            object,
            subject,
            capability,
        } => apply_revoke(
            state, context, object, subject, capability, expected, changed,
        ),
        Operation::Tombstone { object } => {
            apply_tombstone(state, context, object, expected, changed)
        }
    }
}

fn apply_create(
    state: &mut ShardState,
    context: AccessContext,
    request: CreateObject,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
    created: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    if state.objects.contains_key(&request.id) {
        return Err(OmsError::InvalidOperation("ObjectId already exists"));
    }
    if request.links.keys().any(String::is_empty) {
        return Err(OmsError::InvalidOperation("link name cannot be empty"));
    }
    if request.type_id == CORE_NAMESPACE_TYPE {
        for name in request.links.keys() {
            validate_name(name)?;
        }
    }
    for target in request.links.values() {
        if *target != request.id {
            active_record(state, *target)?;
        }
    }
    if let Some(parent) = request.parent {
        if !created.contains(&parent) {
            require_expected(expected, parent)?;
        }
        let parent_record = active_record_mut(state, parent)?;
        parent_record.require(context, Capability::CreateChild)?;
        parent_record.children.insert(request.id);
        changed.insert(parent);
    }
    let id = request.id;
    let type_id = request.type_id;
    state.objects.insert(
        id,
        ObjectRecord {
            header: ObjectHeader {
                id,
                type_id: request.type_id,
                parent_id: request.parent,
                version: ObjectVersion::default(),
                lifecycle: LifecycleState::Active,
            },
            retired_at_unix_ms: None,
            state: Arc::from(request.state),
            children: BTreeSet::new(),
            links: request.links,
            capabilities: request.capabilities,
            policy: AccessPolicy {
                owner: context.subject,
                grants: request.initial_grants,
            },
        },
    );
    state.by_type.entry(type_id).or_default().insert(id);
    changed.insert(id);
    created.insert(id);
    Ok(())
}

fn apply_update(
    state: &mut ShardState,
    context: AccessContext,
    object: ObjectId,
    data: Vec<u8>,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    require_expected(expected, object)?;
    let record = active_record_mut(state, object)?;
    record.require(context, Capability::ReplaceValue)?;
    record.state = Arc::from(data);
    changed.insert(object);
    Ok(())
}

fn apply_set_link(
    state: &mut ShardState,
    context: AccessContext,
    source: ObjectId,
    name: String,
    target: ObjectId,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    require_expected(expected, source)?;
    if name.is_empty() {
        return Err(OmsError::InvalidOperation("link name cannot be empty"));
    }
    active_record(state, target)?;
    let record = active_record_mut(state, source)?;
    if record.header.type_id == CORE_NAMESPACE_TYPE {
        validate_name(&name)?;
    }
    record.require(context, Capability::Link)?;
    record.links.insert(name, target);
    changed.insert(source);
    Ok(())
}

fn apply_remove_link(
    state: &mut ShardState,
    context: AccessContext,
    source: ObjectId,
    name: &str,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    require_expected(expected, source)?;
    let record = active_record_mut(state, source)?;
    if record.header.type_id == CORE_NAMESPACE_TYPE {
        validate_name(name)?;
    }
    record.require(context, Capability::Link)?;
    record.links.remove(name);
    changed.insert(source);
    Ok(())
}

fn apply_reparent(
    state: &mut ShardState,
    context: AccessContext,
    child: ObjectId,
    new_parent: Option<ObjectId>,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    require_expected(expected, child)?;
    let old_parent = active_record(state, child)?.header.parent_id;
    active_record(state, child)?.require(context, Capability::Reparent)?;

    if let Some(parent) = old_parent {
        require_expected(expected, parent)?;
        let parent_record = active_record_mut(state, parent)?;
        parent_record.require(context, Capability::Reparent)?;
        parent_record.children.remove(&child);
        changed.insert(parent);
    }
    if let Some(parent) = new_parent {
        require_expected(expected, parent)?;
        let parent_record = active_record_mut(state, parent)?;
        parent_record.require(context, Capability::Reparent)?;
        parent_record.children.insert(child);
        changed.insert(parent);
    }
    active_record_mut(state, child)?.header.parent_id = new_parent;
    changed.insert(child);
    Ok(())
}

fn apply_grant(
    state: &mut ShardState,
    context: AccessContext,
    object: ObjectId,
    subject: SubjectId,
    capability: Capability,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    require_expected(expected, object)?;
    let record = active_record_mut(state, object)?;
    record.require(context, Capability::ManagePolicy)?;
    if !record.capabilities.contains(&capability) {
        return Err(OmsError::InvalidOperation(
            "cannot grant a capability unsupported by the object",
        ));
    }
    record
        .policy
        .grants
        .entry(subject)
        .or_default()
        .insert(capability);
    changed.insert(object);
    Ok(())
}

fn apply_revoke(
    state: &mut ShardState,
    context: AccessContext,
    object: ObjectId,
    subject: SubjectId,
    capability: Capability,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    require_expected(expected, object)?;
    let record = active_record_mut(state, object)?;
    record.require(context, Capability::ManagePolicy)?;
    if let Some(capabilities) = record.policy.grants.get_mut(&subject) {
        capabilities.remove(&capability);
        if capabilities.is_empty() {
            record.policy.grants.remove(&subject);
        }
    }
    changed.insert(object);
    Ok(())
}

fn apply_tombstone(
    state: &mut ShardState,
    context: AccessContext,
    object: ObjectId,
    expected: &BTreeMap<ObjectId, ObjectVersion>,
    changed: &mut BTreeSet<ObjectId>,
) -> Result<(), OmsError> {
    require_expected(expected, object)?;
    let object_record = active_record(state, object)?;
    if !object_record.children.is_empty() {
        return Err(OmsError::InvalidOperation(
            "an object with children must be reparented before tombstoning",
        ));
    }
    let parent = object_record.header.parent_id;
    object_record.require(context, Capability::Retire)?;
    if unresolved_effect_state(object_record.header.type_id, &object_record.state) {
        return Err(OmsError::InvalidOperation(
            "an unresolved Effect cannot be tombstoned",
        ));
    }
    let retired_at_unix_ms = unix_time_millis()?;
    if let Some(parent) = parent {
        require_expected(expected, parent)?;
        let parent_record = active_record_mut(state, parent)?;
        parent_record.require(context, Capability::Reparent)?;
        parent_record.children.remove(&object);
        changed.insert(parent);
    }
    let record = active_record_mut(state, object)?;
    record.header.parent_id = None;
    record.header.lifecycle = LifecycleState::Tombstoned;
    record.retired_at_unix_ms = Some(retired_at_unix_ms);
    changed.insert(object);
    Ok(())
}

fn active_record(state: &ShardState, object: ObjectId) -> Result<&ObjectRecord, OmsError> {
    let record = state
        .objects
        .get(&object)
        .ok_or(OmsError::NotFound(object))?;
    if record.header.lifecycle == LifecycleState::Tombstoned {
        return Err(OmsError::InvalidLifecycle {
            object,
            state: record.header.lifecycle,
        });
    }
    Ok(record)
}

fn active_record_mut(
    state: &mut ShardState,
    object: ObjectId,
) -> Result<&mut ObjectRecord, OmsError> {
    let record = state
        .objects
        .get_mut(&object)
        .ok_or(OmsError::NotFound(object))?;
    if record.header.lifecycle == LifecycleState::Tombstoned {
        return Err(OmsError::InvalidLifecycle {
            object,
            state: record.header.lifecycle,
        });
    }
    Ok(record)
}

fn combine_shards<'a>(
    shards: impl IntoIterator<Item = &'a ShardState>,
) -> Result<ShardState, OmsError> {
    let mut combined = ShardState::default();
    for shard in shards {
        for (&object, record) in &shard.objects {
            if combined.objects.insert(object, record.clone()).is_some() {
                return Err(OmsError::InvalidOperation(
                    "an ObjectId exists in more than one shard",
                ));
            }
            combined
                .by_type
                .entry(record.header.type_id)
                .or_default()
                .insert(object);
        }
    }
    Ok(combined)
}

fn partition_shards(state: ShardState, directory: &FixedDirectory) -> Vec<ShardState> {
    let mut shards = (0..directory.shard_count())
        .map(|_| ShardState::default())
        .collect::<Vec<_>>();
    for (object, record) in state.objects {
        let shard = &mut shards[directory.locate(object).get() as usize];
        shard
            .by_type
            .entry(record.header.type_id)
            .or_default()
            .insert(object);
        shard.objects.insert(object, record);
    }
    shards
}

fn validate_parent_graph(state: &ShardState) -> Result<(), OmsError> {
    for (&start, record) in &state.objects {
        if record.header.lifecycle == LifecycleState::Tombstoned
            && (record.header.parent_id.is_some() || !record.children.is_empty())
        {
            return Err(OmsError::InvalidOperation(
                "tombstoned objects cannot retain parent or child relationships",
            ));
        }
        if let Some(parent) = record.header.parent_id {
            let parent_record = state
                .objects
                .get(&parent)
                .ok_or(OmsError::NotFound(parent))?;
            if !parent_record.children.contains(&start) {
                return Err(OmsError::InvalidOperation(
                    "parent header and child index are inconsistent",
                ));
            }
        }
        for child in &record.children {
            let child_record = state.objects.get(child).ok_or(OmsError::NotFound(*child))?;
            if child_record.header.parent_id != Some(start) {
                return Err(OmsError::InvalidOperation(
                    "child index and parent header are inconsistent",
                ));
            }
        }

        let mut seen = BTreeSet::new();
        let mut current = Some(start);
        while let Some(object) = current {
            if !seen.insert(object) {
                return Err(OmsError::ParentCycle);
            }
            current = state
                .objects
                .get(&object)
                .and_then(|record| record.header.parent_id);
        }
    }
    Ok(())
}

fn validate_type_index(state: &ShardState) -> Result<(), OmsError> {
    for (&object, record) in &state.objects {
        if !state
            .by_type
            .get(&record.header.type_id)
            .is_some_and(|objects| objects.contains(&object))
        {
            return Err(OmsError::InvalidOperation(
                "object and type index are inconsistent",
            ));
        }
    }
    for (&type_id, objects) in &state.by_type {
        for object in objects {
            if state
                .objects
                .get(object)
                .is_none_or(|record| record.header.type_id != type_id)
            {
                return Err(OmsError::InvalidOperation(
                    "type index and object are inconsistent",
                ));
            }
        }
    }
    Ok(())
}

fn validate_dynamic_types(state: &ShardState, builtins: &TypeRegistry) -> Result<(), OmsError> {
    let mut ids = builtins.by_id.keys().copied().collect::<BTreeSet<_>>();
    let mut names = builtins.by_name.keys().cloned().collect::<BTreeSet<_>>();
    for object in state
        .by_type
        .get(&TYPE_DESCRIPTOR_TYPE)
        .into_iter()
        .flatten()
    {
        let record = state
            .objects
            .get(object)
            .ok_or(OmsError::NotFound(*object))?;
        if record.header.lifecycle == LifecycleState::Tombstoned {
            continue;
        }
        let descriptor = decode_type_descriptor(&record.state)?;
        if descriptor.name.is_empty() || descriptor.name.contains('\0') {
            return Err(OmsError::InvalidOperation(
                "Type Descriptor name must be non-empty and contain no NUL",
            ));
        }
        if !ids.insert(descriptor.id) || !names.insert(descriptor.name) {
            return Err(OmsError::InvalidOperation(
                "Type Descriptor id or name is duplicated",
            ));
        }
    }
    Ok(())
}

fn all_capabilities() -> BTreeSet<Capability> {
    [
        Capability::ViewValue,
        Capability::ReplaceValue,
        Capability::CreateChild,
        Capability::Invoke,
        Capability::Link,
        Capability::Reparent,
        Capability::Retire,
        Capability::Inspect,
        Capability::ManagePolicy,
    ]
    .into_iter()
    .collect()
}

fn validate_name(name: &str) -> Result<(), OmsError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        Err(OmsError::InvalidName(name.to_owned()))
    } else {
        Ok(())
    }
}

fn committed_version(result: &CommitResult, object: ObjectId) -> Result<ObjectVersion, OmsError> {
    result
        .versions
        .get(&object)
        .copied()
        .ok_or(OmsError::InvalidOperation("commit omitted modified object"))
}

fn persist_file_snapshot(path: &Path, bytes: &[u8]) -> Result<(), OmsError> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(storage_error)?;
    }
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary).map_err(storage_error)?;
    file.write_all(bytes).map_err(storage_error)?;
    file.sync_all().map_err(storage_error)?;
    drop(file);
    fs::rename(&temporary, path).map_err(storage_error)?;
    sync_parent_directory(path)
}

fn persist_generation_manifest(path: &Path, bytes: &[u8]) -> Result<bool, OmsError> {
    if let Some(parent) = path.parent().filter(|value| !value.as_os_str().is_empty()) {
        fs::create_dir_all(parent).map_err(storage_error)?;
    }
    let temporary = temporary_path(path);
    let mut file = File::create(&temporary).map_err(storage_error)?;
    file.write_all(bytes).map_err(storage_error)?;
    file.sync_all().map_err(storage_error)?;
    drop(file);
    fs::rename(&temporary, path).map_err(storage_error)?;
    // `false` still means the atomic rename is visible. The caller keeps the
    // old generation intact unless the directory entry is confirmed durable.
    Ok(sync_parent_directory(path).is_ok())
}

fn append_wal(path: &Path, payload: &[u8]) -> Result<(), OmsError> {
    repair_wal_tail(path)?;
    let length = u64::try_from(payload.len())
        .map_err(|_| OmsError::Storage("WAL record is too large".to_owned()))?;
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .map_err(storage_error)?;
    file.write_all(WAL_MAGIC).map_err(storage_error)?;
    file.write_all(&length.to_le_bytes())
        .map_err(storage_error)?;
    file.write_all(payload).map_err(storage_error)?;
    file.write_all(&wal_checksum(payload).to_le_bytes())
        .map_err(storage_error)?;
    file.sync_all().map_err(storage_error)
}

fn repair_wal_tail(path: &Path) -> Result<(), OmsError> {
    if !path.exists() {
        return Ok(());
    }
    let bytes = fs::read(path).map_err(storage_error)?;
    let mut position = 0_usize;
    while bytes.len().saturating_sub(position) >= 12 {
        if &bytes[position..position + 4] != WAL_MAGIC {
            return Err(corruption("invalid Object Store WAL magic"));
        }
        let length = u64::from_le_bytes(
            bytes[position + 4..position + 12]
                .try_into()
                .map_err(|_| corruption("truncated Object Store WAL length"))?,
        );
        let length = usize::try_from(length)
            .map_err(|_| corruption("Object Store WAL record is too large"))?;
        let Some(record_end) = position
            .checked_add(12)
            .and_then(|start| start.checked_add(length))
        else {
            return Err(corruption("Object Store WAL length overflow"));
        };
        let Some(checksum_end) = record_end.checked_add(8) else {
            return Err(corruption("Object Store WAL length overflow"));
        };
        if checksum_end > bytes.len() {
            break;
        }
        let payload = &bytes[position + 12..record_end];
        let expected = u64::from_le_bytes(
            bytes[record_end..checksum_end]
                .try_into()
                .map_err(|_| corruption("truncated Object Store WAL checksum"))?,
        );
        if wal_checksum(payload) != expected {
            return Err(corruption("Object Store WAL checksum mismatch"));
        }
        position = checksum_end;
    }
    if position < bytes.len() {
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(storage_error)?;
        file.set_len(
            u64::try_from(position)
                .map_err(|_| OmsError::Storage("WAL offset does not fit in u64".to_owned()))?,
        )
        .map_err(storage_error)?;
        file.sync_all().map_err(storage_error)?;
    }
    Ok(())
}

fn load_wal(path: &Path, mut latest: Option<Vec<u8>>) -> Result<Option<Vec<u8>>, OmsError> {
    if !path.exists() {
        return Ok(latest);
    }
    let bytes = fs::read(path).map_err(storage_error)?;
    let mut position = 0_usize;
    let mut payloads = Vec::new();
    while bytes.len().saturating_sub(position) >= 12 {
        if &bytes[position..position + 4] != WAL_MAGIC {
            return Err(corruption("invalid Object Store WAL magic"));
        }
        let length = u64::from_le_bytes(
            bytes[position + 4..position + 12]
                .try_into()
                .map_err(|_| corruption("truncated Object Store WAL length"))?,
        );
        let length = usize::try_from(length)
            .map_err(|_| corruption("Object Store WAL record is too large"))?;
        let Some(record_end) = position
            .checked_add(12)
            .and_then(|start| start.checked_add(length))
        else {
            return Err(corruption("Object Store WAL length overflow"));
        };
        let Some(checksum_end) = record_end.checked_add(8) else {
            return Err(corruption("Object Store WAL length overflow"));
        };
        if checksum_end > bytes.len() {
            break;
        }
        let payload = &bytes[position + 12..record_end];
        let expected = u64::from_le_bytes(
            bytes[record_end..checksum_end]
                .try_into()
                .map_err(|_| corruption("truncated Object Store WAL checksum"))?,
        );
        if wal_checksum(payload) != expected {
            return Err(corruption("Object Store WAL checksum mismatch"));
        }
        payloads.push(payload);
        position = checksum_end;
    }
    if position < bytes.len() {
        // A torn final frame was never a committed transaction. Remove it
        // before future appends, otherwise the next valid WAL frame would be
        // hidden behind a permanently malformed tail.
        let file = OpenOptions::new()
            .write(true)
            .open(path)
            .map_err(storage_error)?;
        file.set_len(
            u64::try_from(position)
                .map_err(|_| OmsError::Storage("WAL offset does not fit in u64".to_owned()))?,
        )
        .map_err(storage_error)?;
        file.sync_all().map_err(storage_error)?;
    }
    let start = payloads
        .iter()
        .rposition(|payload| payload.first() == Some(&3));
    if let Some(index) = start {
        latest = Some(payloads[index][1..].to_vec());
        for payload in &payloads[index + 1..] {
            latest = Some(apply_wal_payload(latest.as_deref(), payload)?);
        }
    } else {
        for payload in payloads {
            latest = Some(apply_wal_payload(latest.as_deref(), payload)?);
        }
    }
    Ok(latest)
}

fn encode_wal_payload(previous: Option<&[u8]>, snapshot: &[u8]) -> Result<Vec<u8>, OmsError> {
    let mut full = Vec::with_capacity(snapshot.len().saturating_add(1));
    full.push(0);
    full.extend_from_slice(snapshot);
    let Some(previous) = previous.filter(|value| value.len() == snapshot.len()) else {
        return compress_wal_payload(full);
    };

    let mut runs = Vec::<(usize, &[u8])>::new();
    let mut position = 0_usize;
    while position < snapshot.len() {
        if previous[position] == snapshot[position] {
            position += 1;
            continue;
        }
        let start = position;
        while position < snapshot.len() && previous[position] != snapshot[position] {
            position += 1;
        }
        runs.push((start, &snapshot[start..position]));
    }

    let mut delta = Vec::new();
    delta.push(1);
    delta.extend_from_slice(&wal_checksum(previous).to_le_bytes());
    delta.extend_from_slice(
        &u64::try_from(snapshot.len())
            .map_err(|_| OmsError::Storage("WAL snapshot is too large".to_owned()))?
            .to_le_bytes(),
    );
    delta.extend_from_slice(
        &u32::try_from(runs.len())
            .map_err(|_| OmsError::Storage("WAL has too many delta runs".to_owned()))?
            .to_le_bytes(),
    );
    for (offset, bytes) in runs {
        delta.extend_from_slice(
            &u64::try_from(offset)
                .map_err(|_| OmsError::Storage("WAL delta offset is too large".to_owned()))?
                .to_le_bytes(),
        );
        delta.extend_from_slice(
            &u32::try_from(bytes.len())
                .map_err(|_| OmsError::Storage("WAL delta run is too large".to_owned()))?
                .to_le_bytes(),
        );
        delta.extend_from_slice(bytes);
    }
    compress_wal_payload(if delta.len() < full.len() {
        delta
    } else {
        full
    })
}

fn apply_wal_payload(previous: Option<&[u8]>, payload: &[u8]) -> Result<Vec<u8>, OmsError> {
    let Some((&kind, body)) = payload.split_first() else {
        return Err(corruption("empty Object Store WAL payload"));
    };
    if kind == 2 {
        let mut reader = WalDeltaReader::new(body);
        let length = usize::try_from(reader.u64()?)
            .map_err(|_| corruption("compressed WAL payload is too large"))?;
        let mut expanded = Vec::with_capacity(length);
        while !reader.is_empty() {
            let count = usize::try_from(reader.u32()?)
                .map_err(|_| corruption("compressed WAL run is too large"))?;
            let byte = reader.take(1)?[0];
            if count == 0 || expanded.len().saturating_add(count) > length {
                return Err(corruption("compressed WAL run is invalid"));
            }
            expanded.resize(expanded.len() + count, byte);
        }
        if expanded.len() != length || expanded.first() == Some(&2) {
            return Err(corruption("compressed WAL payload length is invalid"));
        }
        return apply_wal_payload(previous, &expanded);
    }
    if kind == 0 {
        return Ok(body.to_vec());
    }
    if kind == 3 {
        return Ok(body.to_vec());
    }
    if kind != 1 {
        return Err(corruption("invalid Object Store WAL payload kind"));
    }
    let base = previous.ok_or_else(|| corruption("WAL delta has no base snapshot"))?;
    let mut reader = WalDeltaReader::new(body);
    if reader.u64()? != wal_checksum(base) {
        return Err(corruption("WAL delta base checksum mismatch"));
    }
    let length = usize::try_from(reader.u64()?)
        .map_err(|_| corruption("WAL delta snapshot is too large"))?;
    if length != base.len() {
        return Err(corruption("WAL delta base length mismatch"));
    }
    let run_count = usize::try_from(reader.u32()?)
        .map_err(|_| corruption("WAL delta run count is too large"))?;
    let mut snapshot = base.to_vec();
    let mut previous_end = 0_usize;
    for _ in 0..run_count {
        let offset = usize::try_from(reader.u64()?)
            .map_err(|_| corruption("WAL delta offset is too large"))?;
        let bytes = reader.bytes()?;
        let end = offset
            .checked_add(bytes.len())
            .ok_or_else(|| corruption("WAL delta range overflow"))?;
        if offset < previous_end || end > snapshot.len() {
            return Err(corruption("WAL delta range is invalid"));
        }
        snapshot[offset..end].copy_from_slice(bytes);
        previous_end = end;
    }
    if !reader.is_empty() {
        return Err(corruption("trailing Object Store WAL delta data"));
    }
    Ok(snapshot)
}

fn compress_wal_payload(payload: Vec<u8>) -> Result<Vec<u8>, OmsError> {
    let mut compressed = Vec::new();
    compressed.push(2);
    compressed.extend_from_slice(
        &u64::try_from(payload.len())
            .map_err(|_| OmsError::Storage("WAL payload is too large".to_owned()))?
            .to_le_bytes(),
    );
    let mut position = 0_usize;
    while position < payload.len() {
        let byte = payload[position];
        let start = position;
        while position < payload.len()
            && payload[position] == byte
            && position - start < u32::MAX as usize
        {
            position += 1;
        }
        compressed.extend_from_slice(
            &u32::try_from(position - start)
                .map_err(|_| OmsError::Storage("WAL compression run is too large".to_owned()))?
                .to_le_bytes(),
        );
        compressed.push(byte);
    }
    Ok(if compressed.len() < payload.len() {
        compressed
    } else {
        payload
    })
}

struct WalDeltaReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> WalDeltaReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], OmsError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| corruption("WAL delta length overflow"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| corruption("truncated Object Store WAL delta"))?;
        self.position = end;
        Ok(value)
    }

    fn u32(&mut self) -> Result<u32, OmsError> {
        Ok(u32::from_le_bytes(
            self.take(4)?
                .try_into()
                .map_err(|_| corruption("truncated WAL delta u32"))?,
        ))
    }

    fn u64(&mut self) -> Result<u64, OmsError> {
        Ok(u64::from_le_bytes(
            self.take(8)?
                .try_into()
                .map_err(|_| corruption("truncated WAL delta u64"))?,
        ))
    }

    fn bytes(&mut self) -> Result<&'a [u8], OmsError> {
        let length =
            usize::try_from(self.u32()?).map_err(|_| corruption("WAL delta run is too large"))?;
        self.take(length)
    }

    const fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}

fn wal_checksum(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(0x0000_0100_0000_01b3)
    })
}

fn encode_snapshot(state: &ShardState) -> Result<Vec<u8>, OmsError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(SNAPSHOT_MAGIC);
    bytes.extend_from_slice(RETIREMENT_TIME_EXTENSION);
    snapshot_u32(&mut bytes, snapshot_len(state.objects.len())?);
    for record in state.objects.values() {
        snapshot_u128(&mut bytes, record.header.id.as_u128());
        snapshot_u128(&mut bytes, record.header.type_id.as_u128());
        match record.header.parent_id {
            Some(parent) => {
                bytes.push(1);
                snapshot_u128(&mut bytes, parent.as_u128());
            }
            None => bytes.push(0),
        }
        snapshot_u64(&mut bytes, record.header.version.get());
        bytes.push(lifecycle_tag(record.header.lifecycle));
        snapshot_u64(&mut bytes, record.retired_at_unix_ms.unwrap_or_default());
        snapshot_bytes(&mut bytes, &record.state)?;

        snapshot_u32(&mut bytes, snapshot_len(record.children.len())?);
        for child in &record.children {
            snapshot_u128(&mut bytes, child.as_u128());
        }
        snapshot_u32(&mut bytes, snapshot_len(record.links.len())?);
        for (name, target) in &record.links {
            snapshot_string(&mut bytes, name)?;
            snapshot_u128(&mut bytes, target.as_u128());
        }
        snapshot_u16(&mut bytes, capability_bits(&record.capabilities));
        snapshot_u128(&mut bytes, record.policy.owner.as_u128());
        snapshot_u32(&mut bytes, snapshot_len(record.policy.grants.len())?);
        for (subject, capabilities) in &record.policy.grants {
            snapshot_u128(&mut bytes, subject.as_u128());
            snapshot_u16(&mut bytes, capability_bits(capabilities));
        }
    }
    Ok(bytes)
}

fn decode_snapshot(bytes: &[u8]) -> Result<ShardState, OmsError> {
    let mut reader = SnapshotReader::new(bytes);
    if reader.take(4)? != SNAPSHOT_MAGIC {
        return Err(corruption("invalid Object Store snapshot magic"));
    }
    let has_retirement_time = reader.peek(4)? == RETIREMENT_TIME_EXTENSION;
    if has_retirement_time {
        reader.take(4)?;
    }
    let count = reader.count()?;
    let mut objects = BTreeMap::new();
    let mut by_type = BTreeMap::<TypeId, BTreeSet<ObjectId>>::new();
    for _ in 0..count {
        let id = ObjectId::from_u128(reader.u128()?);
        let type_id = TypeId::from_u128(reader.u128()?);
        let parent_id = match reader.u8()? {
            0 => None,
            1 => Some(ObjectId::from_u128(reader.u128()?)),
            _ => return Err(corruption("invalid parent marker")),
        };
        let version = ObjectVersion::new(reader.u64()?);
        let lifecycle = decode_lifecycle(reader.u8()?)?;
        let retired_at_unix_ms = if has_retirement_time {
            let timestamp = reader.u64()?;
            match (lifecycle, timestamp) {
                (LifecycleState::Tombstoned, 0) => {
                    return Err(corruption("Tombstone has no retirement timestamp"));
                }
                (LifecycleState::Tombstoned, timestamp) => Some(timestamp),
                (_, 0) => None,
                (_, _) => return Err(corruption("active Object has a retirement timestamp")),
            }
        } else if lifecycle == LifecycleState::Tombstoned {
            // An older unpublished snapshot has no age metadata. Start the
            // seven-day retention window at upgrade time rather than deleting
            // its payload immediately.
            Some(unix_time_millis()?)
        } else {
            None
        };
        let state = Arc::from(reader.bytes()?.to_vec());

        let mut children = BTreeSet::new();
        for _ in 0..reader.count()? {
            children.insert(ObjectId::from_u128(reader.u128()?));
        }
        let mut links = BTreeMap::new();
        for _ in 0..reader.count()? {
            let name = reader.string()?;
            let target = ObjectId::from_u128(reader.u128()?);
            if links.insert(name, target).is_some() {
                return Err(corruption("duplicate link name"));
            }
        }
        let capabilities = decode_capabilities(reader.u16()?)?;
        let owner = SubjectId::from_u128(reader.u128()?);
        let mut grants = BTreeMap::new();
        for _ in 0..reader.count()? {
            let subject = SubjectId::from_u128(reader.u128()?);
            let granted = decode_capabilities(reader.u16()?)?;
            if grants.insert(subject, granted).is_some() {
                return Err(corruption("duplicate policy subject"));
            }
        }
        let record = ObjectRecord {
            header: ObjectHeader {
                id,
                type_id,
                parent_id,
                version,
                lifecycle,
            },
            retired_at_unix_ms,
            state,
            children,
            links,
            capabilities,
            policy: AccessPolicy { owner, grants },
        };
        if objects.insert(id, record).is_some() {
            return Err(corruption("duplicate ObjectId"));
        }
        by_type.entry(type_id).or_default().insert(id);
    }
    if !reader.is_empty() {
        return Err(corruption("trailing Object Store snapshot data"));
    }
    Ok(ShardState { objects, by_type })
}

const fn lifecycle_tag(state: LifecycleState) -> u8 {
    match state {
        LifecycleState::Creating => 0,
        LifecycleState::Active => 1,
        LifecycleState::Suspended => 2,
        LifecycleState::Migrating => 3,
        LifecycleState::Terminating => 4,
        LifecycleState::Tombstoned => 5,
    }
}

fn decode_lifecycle(tag: u8) -> Result<LifecycleState, OmsError> {
    match tag {
        0 => Ok(LifecycleState::Creating),
        1 => Ok(LifecycleState::Active),
        2 => Ok(LifecycleState::Suspended),
        3 => Ok(LifecycleState::Migrating),
        4 => Ok(LifecycleState::Terminating),
        5 => Ok(LifecycleState::Tombstoned),
        _ => Err(corruption("invalid lifecycle tag")),
    }
}

fn capability_bits(capabilities: &BTreeSet<Capability>) -> u16 {
    capabilities.iter().fold(0, |bits, capability| {
        bits | match capability {
            Capability::ViewValue => 1 << 0,
            Capability::ReplaceValue => 1 << 1,
            Capability::Link => 1 << 2,
            Capability::Reparent => 1 << 3,
            Capability::Retire => 1 << 4,
            Capability::Inspect => 1 << 5,
            Capability::ManagePolicy => 1 << 6,
            Capability::CreateChild => 1 << 7,
            Capability::Invoke => 1 << 8,
        }
    })
}

fn decode_capabilities(bits: u16) -> Result<BTreeSet<Capability>, OmsError> {
    if bits & !0x1ff != 0 {
        return Err(corruption("unknown capability bits"));
    }
    let mappings = [
        (1 << 0, Capability::ViewValue),
        (1 << 1, Capability::ReplaceValue),
        (1 << 2, Capability::Link),
        (1 << 3, Capability::Reparent),
        (1 << 4, Capability::Retire),
        (1 << 5, Capability::Inspect),
        (1 << 6, Capability::ManagePolicy),
        (1 << 7, Capability::CreateChild),
        (1 << 8, Capability::Invoke),
    ];
    Ok(mappings
        .into_iter()
        .filter_map(|(mask, capability)| (bits & mask != 0).then_some(capability))
        .collect())
}

fn snapshot_len(value: usize) -> Result<u32, OmsError> {
    if value > MAX_SNAPSHOT_ITEMS {
        return Err(OmsError::Storage("snapshot item limit exceeded".to_owned()));
    }
    u32::try_from(value).map_err(|_| OmsError::Storage("snapshot is too large".to_owned()))
}

fn snapshot_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), OmsError> {
    snapshot_u32(bytes, snapshot_len(value.len())?);
    bytes.extend_from_slice(value);
    Ok(())
}

fn snapshot_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), OmsError> {
    snapshot_bytes(bytes, value.as_bytes())
}

fn snapshot_u16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn snapshot_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn snapshot_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn snapshot_u128(bytes: &mut Vec<u8>, value: u128) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

// `std::io::Result::map_err` supplies the owned error to this adapter.
#[allow(clippy::needless_pass_by_value)]
fn storage_error(error: std::io::Error) -> OmsError {
    OmsError::Storage(error.to_string())
}

fn corruption(message: &str) -> OmsError {
    OmsError::Corruption(message.to_owned())
}

struct SnapshotReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> SnapshotReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], OmsError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| corruption("snapshot position overflow"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| corruption("truncated Object Store snapshot"))?;
        self.position = end;
        Ok(value)
    }

    fn peek(&self, count: usize) -> Result<&'a [u8], OmsError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| corruption("snapshot position overflow"))?;
        self.bytes
            .get(self.position..end)
            .ok_or_else(|| corruption("truncated Object Store snapshot"))
    }

    fn u8(&mut self) -> Result<u8, OmsError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, OmsError> {
        let mut bytes = [0; 2];
        bytes.copy_from_slice(self.take(2)?);
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32, OmsError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, OmsError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn u128(&mut self) -> Result<u128, OmsError> {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(self.take(16)?);
        Ok(u128::from_le_bytes(bytes))
    }

    fn count(&mut self) -> Result<usize, OmsError> {
        let count = usize::try_from(self.u32()?)
            .map_err(|_| corruption("snapshot count is not supported"))?;
        if count > MAX_SNAPSHOT_ITEMS {
            return Err(corruption("snapshot item limit exceeded"));
        }
        Ok(count)
    }

    fn bytes(&mut self) -> Result<&'a [u8], OmsError> {
        let length = self.count()?;
        self.take(length)
    }

    fn string(&mut self) -> Result<String, OmsError> {
        let value = std::str::from_utf8(self.bytes()?)
            .map_err(|_| corruption("snapshot string is not UTF-8"))?;
        Ok(value.to_owned())
    }

    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_old_snapshot_versions() {
        assert!(matches!(
            decode_snapshot(b"OMS1\0\0\0\0"),
            Err(OmsError::Corruption(_))
        ));
    }

    fn owner() -> (SubjectId, AccessContext) {
        let owner = SubjectId::new();
        (owner, AccessContext::new(owner))
    }

    fn legacy_tombstone_snapshot(id: ObjectId, type_id: TypeId, owner: SubjectId) -> Vec<u8> {
        let mut legacy = Vec::new();
        legacy.extend_from_slice(SNAPSHOT_MAGIC);
        snapshot_u32(&mut legacy, 1);
        snapshot_u128(&mut legacy, id.as_u128());
        snapshot_u128(&mut legacy, type_id.as_u128());
        legacy.push(0);
        snapshot_u64(&mut legacy, 9);
        legacy.push(lifecycle_tag(LifecycleState::Tombstoned));
        snapshot_bytes(&mut legacy, b"retained legacy payload").unwrap();
        snapshot_u32(&mut legacy, 0);
        snapshot_u32(&mut legacy, 0);
        snapshot_u16(&mut legacy, capability_bits(&all_capabilities()));
        snapshot_u128(&mut legacy, owner.as_u128());
        snapshot_u32(&mut legacy, 0);
        legacy
    }

    #[test]
    fn legacy_tombstones_get_a_fresh_retention_period_and_keep_metadata() {
        let id = ObjectId::new();
        let type_id = TypeId::new();
        let owner = SubjectId::new();
        let legacy = legacy_tombstone_snapshot(id, type_id, owner);

        let mut state = decode_snapshot(&legacy).unwrap();
        let record = state.objects.get(&id).unwrap();
        let retired_at = record.retired_at_unix_ms.unwrap();
        assert_eq!(record.header.type_id, type_id);
        assert_eq!(record.header.version, ObjectVersion::new(9));
        assert_eq!(record.policy.owner, owner);
        assert_eq!(record.state.as_ref(), b"retained legacy payload");
        assert_eq!(
            next_tombstone_reap_deadline(&state),
            retired_at.saturating_add(TOMBSTONE_RETENTION_MILLIS)
        );

        let encoded = encode_snapshot(&state).unwrap();
        state = decode_snapshot(&encoded).unwrap();
        let record = state.objects.get(&id).unwrap();
        assert_eq!(record.retired_at_unix_ms, Some(retired_at));

        let mut too_early = state.clone();
        let just_before = compact_tombstone_payloads(&mut too_early, retired_at - 1);
        assert_eq!(just_before.objects_compacted, 0);
        assert_eq!(just_before.tombstones_waiting_for_retention, 1);

        let mut expired = state;
        let eligible = compact_tombstone_payloads(&mut expired, retired_at);
        assert_eq!(eligible.objects_compacted, 1);
        assert_eq!(expired.objects[&id].state.len(), 0);
        assert_eq!(expired.objects[&id].header.id, id);
        assert_eq!(expired.objects[&id].header.type_id, type_id);
        assert_eq!(expired.objects[&id].header.version, ObjectVersion::new(9));
        assert_eq!(expired.objects[&id].policy.owner, owner);
        assert_eq!(expired.objects[&id].retired_at_unix_ms, Some(retired_at));
    }

    #[test]
    fn legacy_retirement_time_upgrade_survives_store_restart() {
        let directory = std::env::temp_dir().join(format!("ousject-legacy-{}", ObjectId::new()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("objects.oms");
        let id = ObjectId::new();
        let type_id = TypeId::new();
        let owner = SubjectId::new();
        std::fs::write(&path, legacy_tombstone_snapshot(id, type_id, owner)).unwrap();

        let first = InMemoryObjectManager::open_persistent(&path).unwrap();
        let context = AccessContext::new(owner);
        let first_header = first.inspect(context, id).unwrap();
        let first_retired_at = first
            .shard(id)
            .read()
            .unwrap()
            .objects
            .get(&id)
            .unwrap()
            .retired_at_unix_ms;
        assert!(first_retired_at.is_some());
        drop(first);

        let reopened = InMemoryObjectManager::open_persistent(&path).unwrap();
        assert_eq!(reopened.inspect(context, id).unwrap(), first_header);
        assert_eq!(
            reopened
                .shard(id)
                .read()
                .unwrap()
                .objects
                .get(&id)
                .unwrap()
                .retired_at_unix_ms,
            first_retired_at
        );
        drop(reopened);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn background_reaper_wakes_for_an_expired_tombstone_and_keeps_its_id() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let (owner, context) = owner();
        let object = create(&manager, context, b"payload removed after expiry");
        let version = manager.inspect(context, object).unwrap().version;
        let mut retire = manager.begin(context);
        retire.expect(object, version).tombstone(object);
        manager.commit(retire).unwrap();

        let retired_at = unix_time_millis()
            .unwrap()
            .saturating_sub(TOMBSTONE_RETENTION_MILLIS + 1);
        manager
            .shard(object)
            .write()
            .unwrap()
            .objects
            .get_mut(&object)
            .unwrap()
            .retired_at_unix_ms = Some(retired_at);
        manager.next_tombstone_reap_unix_ms.store(
            retired_at.saturating_add(TOMBSTONE_RETENTION_MILLIS),
            Ordering::Release,
        );

        let reaper = TombstoneReaper::start(&manager).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if manager.shard(object).read().unwrap().objects[&object]
                .state
                .is_empty()
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "reaper did not compact the expired object"
            );
            thread::yield_now();
        }
        drop(reaper);

        let header = manager.inspect(AccessContext::new(owner), object).unwrap();
        assert_eq!(header.lifecycle, LifecycleState::Tombstoned);
        assert_eq!(manager.stats().unwrap().tombstoned_count, 1);
        assert_eq!(
            manager.next_tombstone_reap_unix_ms.load(Ordering::Acquire),
            u64::MAX
        );
        let mut duplicate = manager.begin(context);
        duplicate.create(CreateObject::new(TypeId::new(), b"reuse").with_id(object));
        assert!(matches!(
            manager.commit(duplicate),
            Err(OmsError::InvalidOperation("ObjectId already exists"))
        ));
    }

    fn create(manager: &InMemoryObjectManager, context: AccessContext, state: &[u8]) -> ObjectId {
        let request = CreateObject::new(TypeId::new(), state);
        let id = request.id;
        let mut transaction = manager.begin(context);
        transaction.create(request);
        manager.commit(transaction).unwrap();
        id
    }

    #[test]
    fn immutable_view_survives_new_version() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, context) = owner();
        let object = create(&manager, context, b"zero");
        let old = manager.read(context, object).unwrap();

        let mut transaction = manager.begin(context);
        transaction
            .expect(object, old.header().version)
            .update_state(object, b"one");
        manager.commit(transaction).unwrap();

        let new = manager.read(context, object).unwrap();
        assert_eq!(old.state(), b"zero");
        assert_eq!(new.state(), b"one");
        assert_eq!(new.header().version, old.header().version.next());
    }

    #[test]
    fn optimistic_conflict_preserves_winner() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, context) = owner();
        let object = create(&manager, context, b"zero");
        let version = manager.read(context, object).unwrap().header().version;

        let mut first = manager.begin(context);
        first.expect(object, version).update_state(object, b"first");
        let mut second = manager.begin(context);
        second
            .expect(object, version)
            .update_state(object, b"second");

        manager.commit(first).unwrap();
        assert!(matches!(
            manager.commit(second),
            Err(OmsError::Conflict { .. })
        ));
        assert_eq!(manager.read(context, object).unwrap().state(), b"first");
    }

    #[test]
    fn denied_write_changes_nothing() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, owner_context) = owner();
        let object = create(&manager, owner_context, b"safe");
        let before = manager.read(owner_context, object).unwrap();
        let intruder = AccessContext::new(SubjectId::new());

        let mut transaction = manager.begin(intruder);
        transaction
            .expect(object, before.header().version)
            .update_state(object, b"corrupted");
        assert!(matches!(
            manager.commit(transaction),
            Err(OmsError::Denied {
                capability: Capability::ReplaceValue,
                ..
            })
        ));

        let after = manager.read(owner_context, object).unwrap();
        assert_eq!(after.state(), b"safe");
        assert_eq!(after.header().version, before.header().version);
    }

    #[test]
    fn parent_link_and_tombstone_are_transactional() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, context) = owner();
        let parent = create(&manager, context, b"process");
        let parent_version = manager.read(context, parent).unwrap().header().version;

        let child_request = CreateObject::new(TypeId::new(), b"0".to_vec()).with_parent(parent);
        let child = child_request.id;
        let mut create_child = manager.begin(context);
        create_child
            .expect(parent, parent_version)
            .create(child_request);
        manager.commit(create_child).unwrap();

        let parent_view = manager.read(context, parent).unwrap();
        assert!(parent_view.children().contains(&child));

        let mut link = manager.begin(context);
        link.expect(parent, parent_view.header().version)
            .set_link(parent, "displayed", child);
        manager.commit(link).unwrap();
        assert_eq!(
            manager.read(context, parent).unwrap().links()["displayed"],
            child
        );

        let parent_version = manager.read(context, parent).unwrap().header().version;
        let child_version = manager.read(context, child).unwrap().header().version;
        let mut tombstone = manager.begin(context);
        tombstone
            .expect(parent, parent_version)
            .expect(child, child_version)
            .tombstone(child);
        manager.commit(tombstone).unwrap();

        assert!(
            !manager
                .read(context, parent)
                .unwrap()
                .children()
                .contains(&child)
        );
        assert!(matches!(
            manager.read(context, child),
            Err(OmsError::InvalidLifecycle {
                state: LifecycleState::Tombstoned,
                ..
            })
        ));
    }

    #[test]
    fn capability_can_be_granted_atomically() {
        let manager = InMemoryObjectManager::new(1).unwrap();
        let (_, owner_context) = owner();
        let reader = SubjectId::new();
        let reader_context = AccessContext::new(reader);
        let object = create(&manager, owner_context, b"visible");
        assert!(matches!(
            manager.read(reader_context, object),
            Err(OmsError::Denied { .. })
        ));

        let version = manager
            .read(owner_context, object)
            .unwrap()
            .header()
            .version;
        let mut grant = manager.begin(owner_context);
        grant
            .expect(object, version)
            .grant(object, reader, Capability::ViewValue);
        manager.commit(grant).unwrap();
        assert_eq!(
            manager.read(reader_context, object).unwrap().state(),
            b"visible"
        );
    }
}
