//! Ousject TF virtual machine backed by Process and Variable Objects.

use oms_runtime::{
    AccessContext, CreateObject, CreateSpec, CreationPolicy, InMemoryObjectManager, ObjectQuery,
    Transaction, TypeDescriptor, ValueSchema,
};
use oms_types::{
    CORE_AUTHENTICATION_TYPE, CORE_CHANNEL_TYPE, CORE_COMPILER_TYPE, CORE_CONSOLE_TYPE,
    CORE_EFFECT_TYPE, CORE_INSTANCE_TYPE, CORE_MATH_TYPE, CORE_NAMESPACE_TYPE,
    CORE_OBJECT_STORE_TYPE, CORE_PROCESS_TYPE, CORE_PROGRAM_TYPE, CORE_PROVIDER_REGISTRY_TYPE,
    CORE_SESSION_TYPE, CORE_SYSTEM_TYPE, CORE_TEXT_TYPE, CORE_TIME_TYPE, CORE_TYPE_REGISTRY_TYPE,
    CORE_USER_REGISTRY_TYPE, CORE_USER_TYPE, CORE_VALUE_TYPE, Capability, LOCAL_USER_NAME,
    LifecycleState, NET_RESOLVER_TYPE, ObjectId, OmsError, SubjectId, TypeId, ValueError,
};
use ousject_auth::{AuthService, UserIdentity};
use ousject_provider::{
    EffectRecord, ObjectProvider, ProviderError, ProviderOutcome, ProviderRegistry,
};
use praxis_compiler::compile;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::io::Read;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tf_format::{Program, TfError, Token, Value};

pub use oms_types::SYSTEM_SUBJECT;
pub const PROGRAM_TYPE: TypeId = CORE_PROGRAM_TYPE;
pub const PROCESS_TYPE: TypeId = CORE_PROCESS_TYPE;
pub const CONSOLE_TYPE: TypeId = CORE_CONSOLE_TYPE;
pub const INSTANCE_TYPE: TypeId = CORE_INSTANCE_TYPE;

const PROCESS_MAGIC: &[u8; 4] = b"OPS0";
const PROCESS_RESULT_EXTENSION: &[u8; 4] = b"PRX0";
const PROCESS_RUNTIME_EXTENSION: &[u8; 4] = b"PXT0";
const PROCESS_RETENTION_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_STATE_ITEMS: usize = 1_000_000;
type ProgramCache = BTreeMap<ObjectId, (oms_types::ObjectVersion, Arc<Program>)>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessStatus {
    Running,
    Suspended,
    Halted,
    Terminated,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessState {
    pub program: ObjectId,
    pub subject: SubjectId,
    pub token_position: u32,
    pub stack: Vec<Value>,
    pub variables: BTreeMap<String, ObjectId>,
    pub status: ProcessStatus,
    pub result: Option<Value>,
    pub error: Option<Value>,
    pub wake_at_unix_ms: Option<u64>,
    pub ended_at_unix_ms: Option<u64>,
    pub frames: Vec<CallFrame>,
    pub handlers: Vec<ExceptionHandler>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallFrame {
    pub return_position: u32,
    pub stack_base: u32,
    pub locals: BTreeMap<String, ObjectId>,
    pub receiver: Option<ObjectId>,
    pub class: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExceptionHandler {
    pub catch_position: u32,
    pub error_name: String,
    pub frame_depth: u32,
    pub stack_base: u32,
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmError {
    Oms(OmsError),
    Tf(TfError),
    Value(ValueError),
    InvalidProcessState(String),
    StackUnderflow,
    UndefinedVariable(String),
    TypeError(&'static str),
    DivisionByZero,
    IndexOutOfBounds,
    MissingKey(String),
    MissingProvider(&'static str),
    Provider(String),
    TokenPositionOutOfRange(u32),
    StepLimitExceeded(u64),
}

impl fmt::Display for VmError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

impl std::error::Error for VmError {}

impl From<OmsError> for VmError {
    fn from(error: OmsError) -> Self {
        Self::Oms(error)
    }
}

impl From<TfError> for VmError {
    fn from(error: TfError) -> Self {
        Self::Tf(error)
    }
}

impl From<ValueError> for VmError {
    fn from(error: ValueError) -> Self {
        Self::Value(error)
    }
}

impl From<ProviderError> for VmError {
    fn from(error: ProviderError) -> Self {
        Self::Provider(error.to_string())
    }
}

pub trait ConsoleProvider: fmt::Debug + Send + Sync {
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

    /// Reads one UTF-8 line and removes its trailing newline.
    ///
    /// # Errors
    ///
    /// Returns a provider-specific message when console input is unavailable.
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

    fn process_ended(&self, process: ObjectId) {
        self.driver.release_process(process);
    }

    fn capabilities(&self) -> BTreeSet<String> {
        BTreeSet::from([
            "print".to_owned(),
            "println".to_owned(),
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

#[derive(Debug)]
pub struct VirtualMachine {
    manager: Arc<InMemoryObjectManager>,
    context: AccessContext,
    console_provider: Option<ObjectId>,
    kernel_services: BTreeMap<String, ObjectId>,
    providers: Arc<ProviderRegistry>,
    program_cache: Arc<Mutex<ProgramCache>>,
    process_reaper: Arc<Mutex<Option<mpsc::Sender<ProcessReaperMessage>>>>,
}

impl VirtualMachine {
    #[must_use]
    pub fn new(manager: Arc<InMemoryObjectManager>) -> Self {
        Self {
            manager,
            context: AccessContext::new(SYSTEM_SUBJECT),
            console_provider: None,
            kernel_services: BTreeMap::new(),
            providers: Arc::new(ProviderRegistry::new()),
            program_cache: Arc::new(Mutex::new(BTreeMap::new())),
            process_reaper: Arc::new(Mutex::new(None)),
        }
    }

    /// Connects a discovered Console Object to this VM boot.
    ///
    /// Persisting a Console Object is not enough to make it usable: a hardware
    /// provider must rediscover and connect it on every boot.
    ///
    /// # Errors
    ///
    /// Returns an error when the Object is unavailable or is not a Console.
    pub fn with_console(
        manager: Arc<InMemoryObjectManager>,
        console: ObjectId,
        driver: Arc<dyn ConsoleProvider>,
    ) -> Result<Self, VmError> {
        let context = AccessContext::new(SYSTEM_SUBJECT);
        if manager.inspect(context, console)?.type_id != CONSOLE_TYPE {
            return Err(VmError::TypeError(
                "console provider has the wrong Object type",
            ));
        }
        let providers = Arc::new(ProviderRegistry::new());
        providers.register(Arc::new(ConsoleObjectProvider {
            object: console,
            driver,
            completed_this_boot: Mutex::new(BTreeMap::new()),
            secrets_this_boot: Mutex::new(BTreeMap::new()),
        }))?;
        providers.register(Arc::new(TimeObjectProvider {
            started: Instant::now(),
            completed: Mutex::new(BTreeMap::new()),
        }))?;
        let kernel_services = Self::publish_kernel_services(&manager)?;
        Ok(Self {
            manager,
            context,
            console_provider: Some(console),
            kernel_services,
            providers,
            program_cache: Arc::new(Mutex::new(BTreeMap::new())),
            process_reaper: Arc::new(Mutex::new(None)),
        })
    }

    /// Registers a domain-capability Provider for this boot.
    ///
    /// # Errors
    ///
    /// Returns an error if another Provider already owns the same Type.
    pub fn register_provider(&self, provider: Arc<dyn ObjectProvider>) -> Result<(), VmError> {
        self.providers.register(provider).map_err(VmError::from)
    }

    /// Publishes or reuses the Object representing a console found by a driver.
    ///
    /// This trusted entry point is for hardware providers. Praxis programs
    /// cannot create `ProviderOnly` Objects.
    ///
    /// # Errors
    ///
    /// Returns an error when querying, encoding or committing the Object fails.
    pub fn publish_console(
        manager: &Arc<InMemoryObjectManager>,
        state: &Value,
    ) -> Result<ObjectId, VmError> {
        let context = AccessContext::new(SYSTEM_SUBJECT);
        if let Some(console) = manager
            .query(context, &ObjectQuery::new().with_type(CONSOLE_TYPE))?
            .first()
        {
            return Ok(console.id);
        }
        let request = CreateObject::new(CONSOLE_TYPE, state.encode()?);
        let console = request.id;
        let mut transaction = manager.begin(context);
        transaction.create(request);
        manager.commit(transaction)?;
        Ok(console)
    }

    /// Publishes or reuses one Provider-owned Object discovered this boot.
    ///
    /// # Errors
    ///
    /// Returns an error for an unknown/public Type, invalid state, or failed
    /// atomic persistence.
    pub fn publish_provider_object(
        manager: &Arc<InMemoryObjectManager>,
        type_id: TypeId,
        state: &Value,
    ) -> Result<ObjectId, VmError> {
        let context = AccessContext::new(SYSTEM_SUBJECT);
        let descriptor = manager.type_by_id(type_id)?;
        if descriptor.creation != oms_runtime::CreationPolicy::ProviderOnly {
            return Err(VmError::TypeError(
                "discovered Provider Object must use a Provider-only Type",
            ));
        }
        if let Some(object) = manager
            .query(context, &ObjectQuery::new().with_type(type_id))?
            .first()
        {
            return Ok(object.id);
        }
        let mut request = CreateObject::new(type_id, state.encode()?);
        request.capabilities = descriptor.capabilities;
        let object = request.id;
        let mut transaction = manager.begin(context);
        transaction.create(request);
        manager.commit(transaction)?;
        Ok(object)
    }

    /// Publishes the singleton Objects through which Praxis reaches kernel
    /// services. Their methods are implemented by the VM/OMS, not userland.
    fn publish_kernel_services(
        manager: &Arc<InMemoryObjectManager>,
    ) -> Result<BTreeMap<String, ObjectId>, VmError> {
        let context = AccessContext::new(SYSTEM_SUBJECT);
        let services = [
            ("system", CORE_SYSTEM_TYPE),
            ("authentication", CORE_AUTHENTICATION_TYPE),
            ("users", CORE_USER_REGISTRY_TYPE),
            ("compiler", CORE_COMPILER_TYPE),
            ("types", CORE_TYPE_REGISTRY_TYPE),
            ("providers", CORE_PROVIDER_REGISTRY_TYPE),
            ("store", CORE_OBJECT_STORE_TYPE),
            ("math", CORE_MATH_TYPE),
            ("time", CORE_TIME_TYPE),
            ("resolver", NET_RESOLVER_TYPE),
        ];
        let mut published = BTreeMap::new();
        for (name, type_id) in services {
            let object = if let Some(header) = manager
                .query(context, &ObjectQuery::new().with_type(type_id))?
                .first()
            {
                header.id
            } else {
                let state = if name == "math" {
                    Value::Record(BTreeMap::from([
                        ("name".to_owned(), Value::Text(name.to_owned())),
                        (
                            "pi".to_owned(),
                            Value::Float(oms_types::FloatValue::new(std::f64::consts::PI)),
                        ),
                        (
                            "e".to_owned(),
                            Value::Float(oms_types::FloatValue::new(std::f64::consts::E)),
                        ),
                    ]))
                } else {
                    Value::Record(BTreeMap::from([(
                        "name".to_owned(),
                        Value::Text(name.to_owned()),
                    )]))
                };
                let request = CreateObject::new(type_id, state.encode()?);
                let object = request.id;
                let mut transaction = manager.begin(context);
                transaction.create(request);
                manager.commit(transaction)?;
                object
            };
            published.insert(name.to_owned(), object);
        }
        for namespace in
            manager.query(context, &ObjectQuery::new().with_type(CORE_NAMESPACE_TYPE))?
        {
            if matches!(
                manager.value(context, namespace.id)?,
                Value::Record(ref fields)
                    if fields.get("name") == Some(&Value::Text("system".to_owned()))
            ) {
                published.insert("programs".to_owned(), namespace.id);
                break;
            }
        }
        Ok(published)
    }

    #[must_use]
    pub fn manager(&self) -> &Arc<InMemoryObjectManager> {
        &self.manager
    }

    /// Creates Program and Process Objects in one atomic transaction.
    ///
    /// # Errors
    ///
    /// Returns an error when TF encoding or the OMS commit fails.
    pub fn create_process(&self, program: &Program) -> Result<ObjectId, VmError> {
        self.create_process_as(program, SYSTEM_SUBJECT)
    }

    /// Creates Program and Process Objects owned by an authenticated Subject.
    ///
    /// # Errors
    ///
    /// Returns an error when TF encoding or the atomic OMS commit fails.
    pub fn create_process_as(
        &self,
        program: &Program,
        subject: SubjectId,
    ) -> Result<ObjectId, VmError> {
        self.grant_kernel_service_access(subject)?;
        let program_state = program.encode()?;
        let mut program_request = CreateObject::new(PROGRAM_TYPE, program_state);
        if let Some(console) = self.console_provider {
            while self.manager.shard_for(program_request.id) != self.manager.shard_for(console) {
                program_request.id = ObjectId::new();
            }
        }
        let program_id = program_request.id;
        let mut process_request = CreateObject::new(
            PROCESS_TYPE,
            encode_process_state(&ProcessState {
                program: program_id,
                subject,
                token_position: 0,
                stack: Vec::new(),
                variables: BTreeMap::new(),
                status: ProcessStatus::Running,
                result: None,
                error: None,
                wake_at_unix_ms: None,
                ended_at_unix_ms: None,
                frames: Vec::new(),
                handlers: Vec::new(),
            })?,
        );
        while self.manager.shard_for(process_request.id) != self.manager.shard_for(program_id) {
            process_request.id = ObjectId::new();
        }
        let process_id = process_request.id;
        process_request = process_request
            .with_link("program", program_id)
            .with_link("process", process_id);
        if let Some(console) = self.console_provider {
            process_request = process_request.with_link("console", console);
        }
        for (name, service) in &self.kernel_services {
            process_request = process_request.with_link(name.clone(), *service);
        }
        let mut transaction = self.manager.begin(AccessContext::new(subject));
        transaction.create(program_request).create(process_request);
        self.manager.commit(transaction)?;
        Ok(process_id)
    }

    fn grant_kernel_service_access(&self, subject: SubjectId) -> Result<(), VmError> {
        if subject == SYSTEM_SUBJECT {
            return Ok(());
        }
        let context = AccessContext::new(SYSTEM_SUBJECT);
        let mut transaction = self.manager.begin(context);
        self.stage_kernel_service_access(subject, &mut transaction)?;
        match self.manager.commit(transaction) {
            Ok(_) | Err(OmsError::InvalidOperation("transaction contains no operations")) => Ok(()),
            Err(error) => Err(error.into()),
        }
    }

    fn stage_kernel_service_access(
        &self,
        subject: SubjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        if subject == SYSTEM_SUBJECT {
            return Ok(());
        }
        let context = AccessContext::new(SYSTEM_SUBJECT);
        let mut objects = self.kernel_services.values().copied().collect::<Vec<_>>();
        if let Some(console) = self.console_provider {
            objects.push(console);
        }
        for object in objects {
            let header = self.manager.inspect(context, object)?;
            transaction
                .expect(object, header.version)
                .grant(object, subject, Capability::Inspect)
                .grant(object, subject, Capability::Invoke)
                .grant(object, subject, Capability::ViewValue);
        }
        Ok(())
    }

    fn require_local(&self) -> Result<(), VmError> {
        if self.context.subject == SYSTEM_SUBJECT {
            Ok(())
        } else {
            Err(VmError::Provider(
                "this kernel capability requires the local identity".to_owned(),
            ))
        }
    }

    fn notify_process_ended(&self, process: ObjectId) -> Result<(), VmError> {
        self.providers
            .process_ended(process)
            .map_err(VmError::from)?;
        if let Ok(notifier) = self.process_reaper.lock() {
            if let Some(notifier) = notifier.as_ref() {
                let _ = notifier.send(ProcessReaperMessage::Wake(process));
            }
        }
        Ok(())
    }

    fn resolve_secret_text(&self, value: &str) -> Result<String, VmError> {
        if !value.starts_with("secret:") {
            return Ok(value.to_owned());
        }
        let provider = self.providers.get(CONSOLE_TYPE)?;
        provider.resolve_secret(value)?.ok_or_else(|| {
            VmError::Provider("secret input expired or belongs to another boot".to_owned())
        })
    }

    /// Reconnects a persisted Process to hardware discovered during this boot.
    ///
    /// # Errors
    ///
    /// Returns an error if no provider was discovered or the Link commit fails.
    pub fn reconnect_hardware(&self, process: ObjectId) -> Result<(), VmError> {
        let console = self
            .console_provider
            .ok_or(VmError::MissingProvider("console"))?;
        let view = self.manager.read(self.context, process)?;
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, view.header().version)
            .set_link(process, "console", console);
        self.manager.commit(transaction)?;
        Ok(())
    }

    /// Marks a Process suspended on a pending Provider Effect as runnable so
    /// the same idempotent Effect can be polled again.
    ///
    /// Returns `true` only when a pending Effect was found and the Process was
    /// made runnable. No new Effect is created.
    ///
    /// # Errors
    ///
    /// Returns an error when the Process or its persisted state cannot be
    /// read or atomically updated.
    pub fn poll_pending_effect(&self, process: ObjectId) -> Result<bool, VmError> {
        let view = self.manager.read(self.context, process)?;
        let mut state = decode_process_state(view.state())?;
        if state.status != ProcessStatus::Suspended || !view.links().contains_key("$effect") {
            return Ok(false);
        }
        state.status = ProcessStatus::Running;
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state)?);
        self.manager.commit(transaction)?;
        Ok(true)
    }

    /// Returns the remaining delay for a timer-suspended Process, if any.
    ///
    /// # Errors
    ///
    /// Returns an error if the Process state cannot be read or decoded.
    pub fn time_until_wake(
        &self,
        process: ObjectId,
    ) -> Result<Option<std::time::Duration>, VmError> {
        let state = self.process_state(process)?;
        Ok(state.wake_at_unix_ms.map(|deadline| {
            std::time::Duration::from_millis(deadline.saturating_sub(unix_time_millis()))
        }))
    }

    /// Makes a timer-suspended Process runnable once its persisted deadline is
    /// reached. The wakeup is an atomic Process-state update.
    ///
    /// # Errors
    ///
    /// Returns an error if the Process state cannot be read, encoded or saved.
    pub fn wake_due_timer(&self, process: ObjectId) -> Result<bool, VmError> {
        let view = self.manager.read(self.context, process)?;
        let mut state = decode_process_state(view.state())?;
        if state.status != ProcessStatus::Suspended
            || state
                .wake_at_unix_ms
                .is_none_or(|deadline| deadline > unix_time_millis())
        {
            return Ok(false);
        }
        state.status = ProcessStatus::Running;
        state.wake_at_unix_ms = None;
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state)?);
        self.manager.commit(transaction)?;
        Ok(true)
    }

    /// Executes or resumes a Process for at most `step_limit` Tokens.
    ///
    /// # Errors
    ///
    /// Returns an error for invalid Process state, invalid operations or OMS
    /// failures. Reaching the step limit returns a report with `Running` status,
    /// so the Process can be resumed later.
    pub fn run(&self, process: ObjectId, step_limit: u64) -> Result<RunReport, VmError> {
        let mut output = Vec::new();
        let mut steps = 0;
        loop {
            let state = self.process_state(process)?;
            if state.status == ProcessStatus::Suspended
                && state
                    .wake_at_unix_ms
                    .is_some_and(|deadline| deadline <= unix_time_millis())
            {
                self.wake_due_timer(process)?;
                continue;
            }
            if state.status != ProcessStatus::Running {
                return Ok(RunReport {
                    process,
                    steps,
                    status: state.status,
                    output,
                });
            }
            if steps >= step_limit {
                return Ok(RunReport {
                    process,
                    steps,
                    status: state.status,
                    output,
                });
            }
            let executor = self.for_subject(state.subject);
            if let Some(line) = executor.step(process, state)? {
                output.push(line);
            }
            steps += 1;
        }
    }

    fn for_subject(&self, subject: SubjectId) -> Self {
        Self {
            manager: Arc::clone(&self.manager),
            context: AccessContext::new(subject),
            console_provider: self.console_provider,
            kernel_services: self.kernel_services.clone(),
            providers: Arc::clone(&self.providers),
            program_cache: Arc::clone(&self.program_cache),
            process_reaper: Arc::clone(&self.process_reaper),
        }
    }

    /// Reads and decodes Process execution state.
    ///
    /// # Errors
    ///
    /// Returns an error if OMS access or state decoding fails.
    pub fn process_state(&self, process: ObjectId) -> Result<ProcessState, VmError> {
        let view = self.manager.read(self.context, process)?;
        decode_process_state(view.state())
    }

    fn program(&self, object: ObjectId) -> Result<Arc<Program>, VmError> {
        let view = self.manager.read(self.context, object)?;
        let version = view.header().version;
        let mut cache = self
            .program_cache
            .lock()
            .map_err(|_| VmError::InvalidProcessState("Program cache unavailable".to_owned()))?;
        if let Some((cached_version, program)) = cache.get(&object) {
            if *cached_version == version {
                return Ok(Arc::clone(program));
            }
        }
        let program = Arc::new(Program::decode(view.state())?);
        cache.insert(object, (version, Arc::clone(&program)));
        Ok(program)
    }

    /// Reads a Process Variable Object by name.
    ///
    /// # Errors
    ///
    /// Returns an error if the variable is absent or its Object state is invalid.
    pub fn variable(&self, process: ObjectId, name: &str) -> Result<Value, VmError> {
        let state = self.process_state(process)?;
        let object = state
            .variables
            .get(name)
            .copied()
            .ok_or_else(|| VmError::UndefinedVariable(name.to_owned()))?;
        let view = self.manager.read(self.context, object)?;
        decode_value_state(view.state())
    }

    fn step(&self, process: ObjectId, state: ProcessState) -> Result<Option<String>, VmError> {
        let has_handler = !state.handlers.is_empty();
        match self.execute_step(process, state) {
            Ok(output) => Ok(output),
            Err(error) if is_catchable(&error) && has_handler => {
                self.catch_error(process, error).map(|()| None)
            }
            Err(error) if is_catchable(&error) => {
                self.fail_process(process, &error)?;
                Err(error)
            }
            Err(error) => Err(error),
        }
    }

    fn fail_process(&self, process: ObjectId, error: &VmError) -> Result<(), VmError> {
        let view = self.manager.read(self.context, process)?;
        let mut state = decode_process_state(view.state())?;
        state.status = ProcessStatus::Failed;
        state.error = Some(error_value(error));
        state.wake_at_unix_ms = None;
        state.ended_at_unix_ms = Some(unix_time_millis());
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state)?);
        self.manager.commit(transaction)?;
        self.notify_process_ended(process)?;
        Ok(())
    }

    fn catch_error(&self, process: ObjectId, error: VmError) -> Result<(), VmError> {
        let process_view = self.manager.read(self.context, process)?;
        let mut state = decode_process_state(process_view.state())?;
        let Some(handler) = state.handlers.pop() else {
            return Err(error);
        };
        state.frames.truncate(
            usize::try_from(handler.frame_depth)
                .map_err(|_| invalid_state("invalid exception frame depth"))?,
        );
        state.stack.truncate(
            usize::try_from(handler.stack_base)
                .map_err(|_| invalid_state("invalid exception stack base"))?,
        );
        state.token_position = handler.catch_position;
        let value = error_value(&error);
        self.commit_store(
            process,
            process_view.header().version,
            &mut state,
            handler.error_name,
            &value,
        )
    }

    // The opcode dispatcher stays centralized so every Token advances and
    // commits Process state through one auditable path.
    #[allow(clippy::too_many_lines)]
    fn execute_step(
        &self,
        process: ObjectId,
        mut state: ProcessState,
    ) -> Result<Option<String>, VmError> {
        let process_view = self.manager.read(self.context, process)?;
        let program = self.program(state.program)?;
        let position = usize::try_from(state.token_position)
            .map_err(|_| VmError::TokenPositionOutOfRange(state.token_position))?;
        let token = program
            .tokens
            .get(position)
            .cloned()
            .ok_or(VmError::TokenPositionOutOfRange(state.token_position))?;
        let next = state
            .token_position
            .checked_add(1)
            .ok_or(VmError::TokenPositionOutOfRange(state.token_position))?;

        match token {
            Token::Push(value) => {
                state.stack.push(value);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Load(name) => {
                let value = self.variable_from_state(&state, &name)?;
                state.stack.push(value);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::LoadIdentity(name) => {
                let binding = binding_id(&state, &name)?;
                state.stack.push(Value::Text(binding.to_string()));
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Store(name) => {
                let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.token_position = next;
                self.commit_store(
                    process,
                    process_view.header().version,
                    &mut state,
                    name,
                    &value,
                )?;
                Ok(None)
            }
            Token::Add | Token::Subtract | Token::Multiply | Token::Divide | Token::Modulo => {
                let right = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let left = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.stack.push(arithmetic(&token, left, right)?);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Equal
            | Token::NotEqual
            | Token::Less
            | Token::LessEqual
            | Token::Greater
            | Token::GreaterEqual => {
                let right = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let left = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state
                    .stack
                    .push(Value::Bool(compare(&token, &left, &right)?));
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Not => {
                let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.stack.push(Value::Bool(!value.is_truthy()));
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Jump(target) => {
                state.token_position = target;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::JumpIfFalse(target) => {
                let condition = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.token_position = if condition.is_truthy() { next } else { target };
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Pop => {
                state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::MakeArray(_)
            | Token::MakeMap(_)
            | Token::IndexGet
            | Token::IndexSet
            | Token::IndexIncrement
            | Token::IndexDecrement
            | Token::Length => {
                execute_collection_token(&token, &mut state.stack)?;
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::RegistryCall { method, arguments } => self.step_call(
                process,
                process_view.header().version,
                &mut state,
                next,
                &method,
                arguments,
                true,
                &program,
            ),
            Token::BindCreated { name, arguments } => {
                let args = pop_call_arguments(&mut state.stack, arguments)?;
                let mut transaction = self.manager.begin(self.context);
                transaction.expect(process, process_view.header().version);
                let (created, output) =
                    self.registry_call(process, state.program, "create", &args, &mut transaction)?;
                debug_assert!(output.is_none());
                bind_name(&mut state, name, object_id(&created)?);
                state.token_position = next;
                transaction.update_state(process, encode_process_state(&state)?);
                self.manager.commit(transaction)?;
                Ok(None)
            }
            Token::BindFound { name, arguments } => {
                let args = pop_call_arguments(&mut state.stack, arguments)?;
                let mut transaction = self.manager.begin(self.context);
                transaction.expect(process, process_view.header().version);
                let (found, output) =
                    self.registry_call(process, state.program, "find", &args, &mut transaction)?;
                debug_assert!(output.is_none());
                bind_name(&mut state, name, object_id(&found)?);
                state.token_position = next;
                transaction.update_state(process, encode_process_state(&state)?);
                self.manager.commit(transaction)?;
                Ok(None)
            }
            Token::ObjectCall { method, arguments } => self.step_call(
                process,
                process_view.header().version,
                &mut state,
                next,
                &method,
                arguments,
                false,
                &program,
            ),
            Token::DefineFunction { end, .. }
            | Token::DefineClass { end, .. }
            | Token::DefineMethod { end, .. } => {
                state.token_position = end;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::CallFunction { name, arguments } => self.enter_function(
                process,
                process_view.header().version,
                &mut state,
                next,
                &program,
                &name,
                arguments,
            ),
            Token::Return => {
                let result = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let frame = state
                    .frames
                    .pop()
                    .ok_or(VmError::TypeError("return outside function"))?;
                state.stack.truncate(
                    usize::try_from(frame.stack_base).map_err(|_| VmError::StackUnderflow)?,
                );
                state.stack.push(result);
                state.token_position = frame.return_position;
                state.handlers.retain(|handler| {
                    usize::try_from(handler.frame_depth)
                        .is_ok_and(|depth| depth <= state.frames.len())
                });
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::GetField(field) => {
                let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let value = self.get_field(&state, &receiver, &field, &program)?;
                state.stack.push(value);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::SetField(field) => {
                let value = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
                state.token_position = next;
                self.commit_field(
                    process,
                    process_view.header().version,
                    &state,
                    &receiver,
                    &field,
                    value,
                    &program,
                )?;
                Ok(None)
            }
            Token::BindLink { name, target } => {
                let target = binding_id(&state, &target)?;
                bind_name(&mut state, name, target);
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::SuperCall { method, arguments } => self.step_super_call(
                process,
                process_view.header().version,
                &mut state,
                next,
                &program,
                &method,
                arguments,
            ),
            Token::BeginTry { catch, error, .. } => {
                state.handlers.push(ExceptionHandler {
                    catch_position: catch,
                    error_name: error,
                    frame_depth: u32::try_from(state.frames.len())
                        .map_err(|_| invalid_state("too many call frames"))?,
                    stack_base: u32::try_from(state.stack.len())
                        .map_err(|_| invalid_state("stack is too large"))?,
                });
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::EndTry { end } => {
                state
                    .handlers
                    .pop()
                    .ok_or(VmError::TypeError("try handler stack is empty"))?;
                state.token_position = end;
                self.commit_process(process, process_view.header().version, &state)?;
                Ok(None)
            }
            Token::Transaction { end } => self.step_transaction(
                process,
                process_view.header().version,
                &mut state,
                &program,
                next,
                end,
            ),
            Token::CommitTransaction => Err(VmError::TypeError(
                "transaction commit marker cannot execute directly",
            )),
            Token::Halt => {
                state.status = ProcessStatus::Halted;
                state.result = state.stack.last().cloned();
                state.error = None;
                state.wake_at_unix_ms = None;
                state.ended_at_unix_ms = Some(unix_time_millis());
                state.token_position = next;
                self.commit_process(process, process_view.header().version, &state)?;
                self.notify_process_ended(process)?;
                Ok(None)
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn step_transaction(
        &self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        program: &Program,
        start: u32,
        end: u32,
    ) -> Result<Option<String>, VmError> {
        let start = usize::try_from(start)
            .map_err(|_| VmError::TypeError("transaction position is too large"))?;
        let commit = usize::try_from(end)
            .map_err(|_| VmError::TypeError("transaction position is too large"))?
            .checked_sub(1)
            .ok_or(VmError::TypeError("invalid transaction range"))?;
        if !matches!(program.tokens.get(commit), Some(Token::CommitTransaction)) {
            return Err(VmError::TypeError("invalid transaction commit marker"));
        }

        let mut stack = Vec::new();
        let mut staged = BTreeMap::<ObjectId, Value>::new();
        let mut created = BTreeMap::<ObjectId, Value>::new();
        for token in program
            .tokens
            .get(start..commit)
            .ok_or(VmError::TypeError("invalid transaction range"))?
        {
            match token {
                Token::Push(value) => stack.push(value.clone()),
                Token::Load(name) => {
                    let object = binding_id(state, name)?;
                    stack.push(self.staged_value(object, &staged, &created)?);
                }
                Token::LoadIdentity(name) => {
                    stack.push(Value::Text(binding_id(state, name)?.to_string()));
                }
                Token::Store(name) => {
                    let value = stack.pop().ok_or(VmError::StackUnderflow)?;
                    let existing = state
                        .frames
                        .last()
                        .and_then(|frame| frame.locals.get(name))
                        .or_else(|| state.variables.get(name))
                        .copied();
                    if let Some(object) = existing {
                        if let Some(current) = created.get_mut(&object) {
                            *current = value;
                        } else {
                            staged.insert(object, value);
                        }
                    } else {
                        let mut request = self.manager.prepare_create(
                            CreateSpec::new("core.value", value.clone()).with_parent(process),
                        )?;
                        while self.manager.shard_for(request.id) != self.manager.shard_for(process)
                        {
                            request.id = ObjectId::new();
                        }
                        let object = request.id;
                        bind_name(state, name.clone(), object);
                        created.insert(object, value);
                    }
                }
                Token::Add | Token::Subtract | Token::Multiply | Token::Divide | Token::Modulo => {
                    let right = stack.pop().ok_or(VmError::StackUnderflow)?;
                    let left = stack.pop().ok_or(VmError::StackUnderflow)?;
                    stack.push(arithmetic(token, left, right)?);
                }
                Token::Equal
                | Token::NotEqual
                | Token::Less
                | Token::LessEqual
                | Token::Greater
                | Token::GreaterEqual => {
                    let right = stack.pop().ok_or(VmError::StackUnderflow)?;
                    let left = stack.pop().ok_or(VmError::StackUnderflow)?;
                    stack.push(Value::Bool(compare(token, &left, &right)?));
                }
                Token::Not => {
                    let value = stack.pop().ok_or(VmError::StackUnderflow)?;
                    stack.push(Value::Bool(!value.is_truthy()));
                }
                Token::MakeArray(_)
                | Token::MakeMap(_)
                | Token::IndexGet
                | Token::IndexSet
                | Token::IndexIncrement
                | Token::IndexDecrement
                | Token::Length => execute_collection_token(token, &mut stack)?,
                Token::GetField(field) => {
                    let receiver = object_id(&stack.pop().ok_or(VmError::StackUnderflow)?)?;
                    let value = self.staged_value(receiver, &staged, &created)?;
                    Self::check_field_visibility(state, receiver, field, program, &value)?;
                    let (Value::Map(fields) | Value::Record(fields)) = value else {
                        return Err(VmError::TypeError("Object value has no fields"));
                    };
                    stack.push(
                        fields
                            .get(field)
                            .cloned()
                            .ok_or_else(|| VmError::MissingKey(field.clone()))?,
                    );
                }
                Token::SetField(field) => {
                    let value = stack.pop().ok_or(VmError::StackUnderflow)?;
                    let receiver = object_id(&stack.pop().ok_or(VmError::StackUnderflow)?)?;
                    let current = self.staged_value(receiver, &staged, &created)?;
                    Self::check_field_visibility(state, receiver, field, program, &current)?;
                    let (Value::Map(mut fields) | Value::Record(mut fields)) = current else {
                        return Err(VmError::TypeError("Object value has no fields"));
                    };
                    if !fields.contains_key(field) {
                        return Err(VmError::MissingKey(field.clone()));
                    }
                    fields.insert(field.clone(), value);
                    let replacement = Value::Record(fields);
                    if let Some(current) = created.get_mut(&receiver) {
                        *current = replacement;
                    } else {
                        staged.insert(receiver, replacement);
                    }
                }
                Token::BindLink { name, target } => {
                    let target = binding_id(state, target)?;
                    bind_name(state, name.clone(), target);
                }
                _ => {
                    return Err(VmError::TypeError(
                        "token is not allowed in an atomic transaction",
                    ));
                }
            }
        }
        if !stack.is_empty() {
            return Err(VmError::TypeError(
                "transaction statements left temporary values",
            ));
        }

        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, process_version);
        for (object, value) in created {
            let mut request = self
                .manager
                .prepare_create(CreateSpec::new("core.value", value).with_parent(process))?;
            request.id = object;
            transaction.create(request);
        }
        for (object, value) in staged {
            let view = self.manager.read(self.context, object)?;
            let encoded = if view.header().type_id == INSTANCE_TYPE {
                value.encode()?
            } else {
                self.manager
                    .prepare_replace_value(self.context, object, &value)?
                    .1
            };
            transaction
                .expect(object, view.header().version)
                .update_state(object, encoded);
        }
        state.token_position = end;
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(None)
    }

    fn staged_value(
        &self,
        object: ObjectId,
        staged: &BTreeMap<ObjectId, Value>,
        created: &BTreeMap<ObjectId, Value>,
    ) -> Result<Value, VmError> {
        staged
            .get(&object)
            .or_else(|| created.get(&object))
            .cloned()
            .map_or_else(
                || self.manager.value(self.context, object).map_err(Into::into),
                Ok,
            )
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn step_call(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        method: &str,
        arguments: u32,
        registry: bool,
        program: &Program,
    ) -> Result<Option<String>, VmError> {
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, version);
        let args = pop_call_arguments(&mut state.stack, arguments)?;
        let mut ended_processes = Vec::new();
        let (result, output) = if registry {
            self.registry_call(process, state.program, method, &args, &mut transaction)?
        } else {
            let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
            let receiver_id = object_id(&receiver)?;
            let receiver_type = self.manager.inspect(self.context, receiver_id)?.type_id;
            if receiver_type == CORE_TIME_TYPE && method == "sleep" {
                self.manager
                    .require_capability(self.context, receiver_id, Capability::Invoke)?;
                let [Value::Integer(milliseconds)] = args.as_slice() else {
                    return Err(VmError::TypeError("time.sleep requires milliseconds"));
                };
                let milliseconds = u64::try_from(*milliseconds)
                    .map_err(|_| VmError::TypeError("sleep duration must not be negative"))?;
                let deadline = unix_time_millis()
                    .checked_add(milliseconds)
                    .ok_or(VmError::TypeError("sleep deadline is too large"))?;
                state.stack.push(Value::Null);
                state.token_position = next;
                state.status = ProcessStatus::Suspended;
                state.wake_at_unix_ms = Some(deadline);
                state.ended_at_unix_ms = None;
                transaction.update_state(process, encode_process_state(state)?);
                self.manager.commit(transaction)?;
                return Ok(None);
            }
            if matches!(
                receiver_type,
                CORE_AUTHENTICATION_TYPE | CORE_USER_REGISTRY_TYPE | CORE_TYPE_REGISTRY_TYPE
            ) {
                // Authentication changes and this Process's instruction/state
                // advance must form one commit. Authorization was checked with
                // the real caller above and again by the domain capability.
                transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
                transaction.expect(process, version);
            } else if receiver_type == CORE_CHANNEL_TYPE && method == "send" {
                for capability in [
                    Capability::Invoke,
                    Capability::ViewValue,
                    Capability::ReplaceValue,
                ] {
                    self.manager
                        .require_capability(self.context, receiver_id, capability)?;
                }
                // Waking a registered waiter is a scheduler action. The caller
                // is checked above, then the trusted VM may update Processes
                // owned by other Subjects in the same atomic commit.
                transaction = self.manager.begin(AccessContext::new(SYSTEM_SUBJECT));
                transaction.expect(process, version);
            }
            let domain_method = self
                .manager
                .type_by_id(receiver_type)?
                .domain_capabilities
                .contains(method);
            if is_base_object_operation(method) && !domain_method {
                let mut explicit = Vec::with_capacity(args.len() + 1);
                explicit.push(receiver.clone());
                explicit.extend(args);
                let (result, output) = if method == "retire" {
                    self.retire_object(process, state, object_id(&receiver)?, &mut transaction)?;
                    (Value::Null, None)
                } else {
                    (
                        self.object_operation(state.program, method, &explicit, &mut transaction)?,
                        None,
                    )
                };
                if matches!(method, "replace" | "link" | "unlink") {
                    (receiver, output)
                } else {
                    (result, output)
                }
            } else if receiver_type == INSTANCE_TYPE {
                return self.enter_method(
                    process,
                    version,
                    state,
                    next,
                    program,
                    &receiver,
                    method,
                    args,
                    None,
                    transaction,
                );
            } else {
                if receiver_type == PROCESS_TYPE && method == "terminate" {
                    ended_processes.push(receiver_id);
                }
                let object = object_id(&receiver)?;
                let type_id = self.manager.inspect(self.context, object)?.type_id;
                if self.providers.get(type_id).is_ok() {
                    return self
                        .step_provider_call(process, version, state, next, object, method, &args);
                }
                self.invoke_object(process, state, &receiver, method, &args, &mut transaction)?
            }
        };
        state.stack.push(result);
        state.token_position = next;
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        for ended_process in ended_processes {
            self.notify_process_ended(ended_process)?;
        }
        Ok(output)
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn step_provider_call(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        object: ObjectId,
        capability: &str,
        arguments: &[Value],
    ) -> Result<Option<String>, VmError> {
        self.manager
            .require_capability(self.context, object, Capability::Invoke)?;
        let target = self.manager.read(self.context, object)?;
        let provider = self
            .providers
            .get(target.header().type_id)
            .map_err(VmError::from)?;
        if !provider.capabilities().contains(capability) {
            return Err(VmError::from(ProviderError::UnsupportedCapability(
                capability.to_owned(),
            )));
        }

        let effect = if let Some(effect) = self
            .manager
            .read(self.context, process)?
            .links()
            .get("$effect")
            .copied()
        {
            let record = EffectRecord::decode(self.manager.read(self.context, effect)?.state())
                .map_err(VmError::from)?;
            if record.process != process
                || record.target != object
                || record.token_position != state.token_position
                || record.capability != capability
                || record.arguments != arguments
            {
                return Err(VmError::InvalidProcessState(
                    "pending Effect does not match the Process token".to_owned(),
                ));
            }
            effect
        } else {
            let effect = ObjectId::new();
            let record = EffectRecord::pending(
                process,
                object,
                state.token_position,
                capability,
                arguments.to_vec(),
            );
            let mut intent = self.manager.begin(self.context);
            intent
                .expect(process, version)
                .create(record.create_object(effect).map_err(VmError::from)?)
                .set_link(process, "$effect", effect);
            self.manager.commit(intent)?;
            effect
        };

        let target_value = Value::decode(target.state())?;
        let outcome = provider.invoke_for_process(
            process,
            object,
            &target_value,
            capability,
            arguments,
            effect,
        );
        let process_view = self.manager.read(self.context, process)?;
        let effect_view = self.manager.read(self.context, effect)?;
        let pending = EffectRecord::decode(effect_view.state()).map_err(VmError::from)?;
        let mut completion = self.manager.begin(self.context);
        completion
            .expect(process, process_view.header().version)
            .expect(effect, effect_view.header().version)
            .remove_link(process, "$effect");
        match outcome {
            Ok(outcome) => {
                completion.update_state(
                    effect,
                    pending
                        .complete(outcome.result.clone())
                        .encode()
                        .map_err(VmError::from)?,
                );
                if let Some(object_state) = outcome.object_state {
                    completion
                        .expect(object, target.header().version)
                        .update_state(object, object_state.encode()?);
                }
                for request in outcome.created {
                    if let Some(parent) = request.parent {
                        let parent_version = self.manager.inspect(self.context, parent)?.version;
                        completion.expect(parent, parent_version);
                    }
                    completion.create(request);
                }
                state.stack.push(outcome.result);
                state.token_position = next;
                completion.update_state(process, encode_process_state(state)?);
                self.manager.commit(completion)?;
                Ok(
                    if target.header().type_id == CONSOLE_TYPE && capability == "println" {
                        arguments.first().map(ToString::to_string)
                    } else {
                        None
                    },
                )
            }
            Err(ProviderError::Pending) => {
                // `step_call` already popped the receiver and arguments. The
                // Process remains on this same call token, so its operand
                // stack must be restored exactly for the idempotent retry.
                state.stack.push(Value::Text(object.to_string()));
                state.stack.extend(arguments.iter().cloned());
                state.status = ProcessStatus::Suspended;
                completion = self.manager.begin(self.context);
                completion
                    .expect(process, process_view.header().version)
                    .update_state(process, encode_process_state(state)?);
                self.manager.commit(completion)?;
                Ok(None)
            }
            Err(error) => {
                completion.update_state(
                    effect,
                    pending
                        .fail(error.to_string())
                        .encode()
                        .map_err(VmError::from)?,
                );
                self.manager.commit(completion)?;
                Err(VmError::from(error))
            }
        }
    }

    fn registry_call(
        &self,
        process: ObjectId,
        program_id: ObjectId,
        method: &str,
        args: &[Value],
        transaction: &mut Transaction,
    ) -> Result<(Value, Option<String>), VmError> {
        let output = match (method, args) {
            ("create", [Value::Text(type_name), value]) => {
                let mut request =
                    self.prepare_object_create(program_id, type_name, value, process)?;
                while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                    request.id = ObjectId::new();
                }
                let process_object = request.type_id == PROCESS_TYPE;
                let id = request.id;
                if process_object {
                    request.links.insert("process".to_owned(), id);
                }
                transaction.create(request);
                Ok(Value::Text(id.to_string()))
            }
            ("create", [Value::Text(type_name), value, parent]) => {
                let parent = object_id(parent)?;
                let parent_version = self.manager.inspect(self.context, parent)?.version;
                let mut request =
                    self.prepare_object_create(program_id, type_name, value, parent)?;
                while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                    request.id = ObjectId::new();
                }
                let process_object = request.type_id == PROCESS_TYPE;
                let id = request.id;
                if process_object {
                    request.links.insert("process".to_owned(), id);
                }
                transaction.expect(parent, parent_version).create(request);
                Ok(Value::Text(id.to_string()))
            }
            ("find", [Value::Text(identity)]) => {
                let object = if let Ok(object) = identity.parse() {
                    object
                } else {
                    self.manager
                        .read(self.context, process)?
                        .links()
                        .get(identity)
                        .copied()
                        .ok_or_else(|| VmError::MissingKey(identity.clone()))?
                };
                self.manager.inspect(self.context, object)?;
                Ok(Value::Text(object.to_string()))
            }
            ("query", [Value::Text(type_name)] | [Value::Text(type_name), Value::Null]) => {
                self.query_objects(program_id, type_name, None)
            }
            ("query", [Value::Text(type_name), Value::Text(capability)]) => {
                self.query_objects(program_id, type_name, Some(capability))
            }
            _ => Err(VmError::TypeError(
                "object only supports create, find and query",
            )),
        }?;
        Ok((output, None))
    }

    fn retire_object(
        &self,
        process: ObjectId,
        state: &mut ProcessState,
        target: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let mut retired = BTreeSet::new();
        let mut discovery_order = Vec::new();
        let mut pending = vec![target];
        while let Some(object) = pending.pop() {
            if !retired.insert(object) {
                continue;
            }
            let view = self.manager.read(self.context, object)?;
            if view.header().type_id == CORE_USER_TYPE
                && is_local_user_value(&self.manager.value(self.context, object)?)
            {
                return Err(VmError::TypeError(
                    "the reserved local User cannot be retired",
                ));
            }
            if view.header().type_id == CORE_EFFECT_TYPE {
                let effect = EffectRecord::decode(view.state())?;
                if effect.status == ousject_provider::EffectStatus::Pending {
                    return Err(VmError::TypeError("a pending Effect cannot be retired"));
                }
            }
            discovery_order.push(object);
            pending.extend(view.children().iter().copied());
        }
        if retired.contains(&process) {
            return Err(VmError::TypeError(
                "a running Process must terminate itself instead of retiring itself",
            ));
        }

        remove_retired_bindings(state, &retired);
        for header in self.manager.list(self.context)? {
            if header.lifecycle == LifecycleState::Tombstoned {
                continue;
            }
            let view = self.manager.read(self.context, header.id)?;
            let mut link_names: BTreeSet<String> = view
                .links()
                .iter()
                .filter(|(_, linked)| retired.contains(linked))
                .map(|(name, _)| name.clone())
                .collect();
            if retired.contains(&header.id) {
                link_names.extend(view.links().keys().cloned());
            }
            if !link_names.is_empty() {
                transaction.expect(header.id, header.version);
                for name in link_names {
                    transaction.remove_link(header.id, name);
                }
            }
            if header.type_id == PROCESS_TYPE
                && header.id != process
                && !retired.contains(&header.id)
            {
                let mut other_state = decode_process_state(view.state())?;
                if remove_retired_bindings(&mut other_state, &retired) {
                    transaction
                        .expect(header.id, header.version)
                        .update_state(header.id, encode_process_state(&other_state)?);
                }
            }
        }

        for object in discovery_order.into_iter().rev() {
            let view = self.manager.read(self.context, object)?;
            transaction.expect(object, view.header().version);
            if let Some(parent) = view.header().parent_id {
                let parent_version = self.manager.inspect(self.context, parent)?.version;
                transaction.expect(parent, parent_version);
            }
            transaction.tombstone(object);
        }
        Ok(())
    }

    fn wait_for_process(&self, process: ObjectId) -> Result<Value, VmError> {
        loop {
            match self.run(process, 1_000_000) {
                Ok(report) if report.status == ProcessStatus::Suspended => {
                    if self.poll_pending_effect(process)? {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                        continue;
                    }
                    if let Some(delay) = self.time_until_wake(process)? {
                        std::thread::sleep(delay);
                        let _ = self.wake_due_timer(process)?;
                        continue;
                    }
                    return Ok(Value::Text("suspended".to_owned()));
                }
                Ok(report) => {
                    return Ok(Value::Text(process_status_name(report.status).to_owned()));
                }
                Err(_error) if self.process_state(process)?.status == ProcessStatus::Failed => {
                    return Ok(Value::Text("failed".to_owned()));
                }
                Err(error) => return Err(error),
            }
        }
    }

    #[allow(clippy::too_many_lines)]
    fn invoke_object(
        &self,
        current_process: ObjectId,
        current_state: &mut ProcessState,
        target: &Value,
        capability: &str,
        arguments: &[Value],
        transaction: &mut Transaction,
    ) -> Result<(Value, Option<String>), VmError> {
        let object = object_id(target)?;
        self.manager
            .require_capability(self.context, object, Capability::Invoke)?;
        let header = self.manager.inspect(self.context, object)?;
        match (header.type_id, capability, arguments) {
            (CORE_AUTHENTICATION_TYPE, "local_initialized", []) => {
                let initialized = AuthService::new(Arc::clone(&self.manager))
                    .users()
                    .map_err(|error| VmError::Provider(error.to_string()))?
                    .iter()
                    .any(|user| user.subject == SYSTEM_SUBJECT && user.name == "local");
                Ok((Value::Bool(initialized), None))
            }
            (CORE_AUTHENTICATION_TYPE, "initialize_local", [Value::Text(password)]) => {
                self.require_local()?;
                let password = self.resolve_secret_text(password)?;
                let identity = AuthService::new(Arc::clone(&self.manager))
                    .stage_initialize_local(&password, transaction)
                    .map_err(|error| VmError::Provider(error.to_string()))?;
                Ok((user_identity_value(&identity), None))
            }
            (CORE_AUTHENTICATION_TYPE, "login", [Value::Text(name), Value::Text(password)]) => {
                let password = self.resolve_secret_text(password)?;
                let session = AuthService::new(Arc::clone(&self.manager))
                    .stage_login(name, &password, transaction)
                    .map_err(|error| VmError::Provider(error.to_string()))?;
                self.stage_kernel_service_access(session.subject, transaction)?;
                self.stage_process_subject_access(
                    current_process,
                    current_state,
                    session.subject,
                    transaction,
                )?;
                current_state.subject = session.subject;
                Ok((
                    Value::Record(BTreeMap::from([
                        ("token".to_owned(), Value::Text(session.token)),
                        (
                            "subject".to_owned(),
                            Value::Text(session.subject.to_string()),
                        ),
                        (
                            "expires_at".to_owned(),
                            Value::Text(session.expires_at.to_string()),
                        ),
                    ])),
                    None,
                ))
            }
            (CORE_AUTHENTICATION_TYPE, "current_user", []) => {
                let identity = AuthService::new(Arc::clone(&self.manager))
                    .users()
                    .map_err(|error| VmError::Provider(error.to_string()))?
                    .into_iter()
                    .find(|identity| identity.subject == self.context.subject)
                    .ok_or_else(|| {
                        VmError::Provider("current Process has no user identity".to_owned())
                    })?;
                Ok((user_identity_value(&identity), None))
            }
            (CORE_AUTHENTICATION_TYPE, "logout", [Value::Text(token)]) => {
                AuthService::new(Arc::clone(&self.manager))
                    .stage_logout(token, transaction)
                    .map_err(|error| VmError::Provider(error.to_string()))?;
                Ok((Value::Null, None))
            }
            (
                CORE_AUTHENTICATION_TYPE,
                "change_password",
                [Value::Text(name), Value::Text(password)],
            ) => {
                let password = self.resolve_secret_text(password)?;
                AuthService::new(Arc::clone(&self.manager))
                    .stage_change_password(self.context.subject, name, &password, transaction)
                    .map_err(|error| VmError::Provider(error.to_string()))?;
                Ok((Value::Null, None))
            }
            (
                CORE_USER_REGISTRY_TYPE,
                "create_user",
                [Value::Text(name), Value::Text(password)],
            ) => {
                self.require_local()?;
                let password = self.resolve_secret_text(password)?;
                let identity = AuthService::new(Arc::clone(&self.manager))
                    .stage_create_user(name, &password, transaction)
                    .map_err(|error| VmError::Provider(error.to_string()))?;
                Ok((user_identity_value(&identity), None))
            }
            (CORE_USER_REGISTRY_TYPE, "users", []) => {
                self.require_local()?;
                let users = AuthService::new(Arc::clone(&self.manager))
                    .users()
                    .map_err(|error| VmError::Provider(error.to_string()))?;
                Ok((
                    Value::Array(users.iter().map(user_identity_value).collect()),
                    None,
                ))
            }
            (CORE_USER_REGISTRY_TYPE, "disable_user", [Value::Text(name)]) => {
                self.require_local()?;
                AuthService::new(Arc::clone(&self.manager))
                    .stage_disable_user(name, transaction)
                    .map_err(|error| VmError::Provider(error.to_string()))?;
                Ok((Value::Null, None))
            }
            (CORE_SYSTEM_TYPE, "status" | "health_check", []) => {
                self.manager.health_check()?;
                let stats = self.manager.stats()?;
                let request = match self.manager.value(self.context, object)? {
                    Value::Record(fields) => fields.get("request").cloned().unwrap_or(Value::Null),
                    _ => Value::Null,
                };
                Ok((
                    Value::Record(BTreeMap::from([
                        ("status".to_owned(), Value::Text("running".to_owned())),
                        ("format_version".to_owned(), Value::Integer(0)),
                        ("request".to_owned(), request),
                        (
                            "objects".to_owned(),
                            Value::Integer(count_integer(stats.object_count)?),
                        ),
                        (
                            "active".to_owned(),
                            Value::Integer(count_integer(stats.active_count)?),
                        ),
                    ])),
                    None,
                ))
            }
            (CORE_SYSTEM_TYPE, "shutdown" | "restart", []) => {
                self.require_local()?;
                self.stage_object_value(
                    object,
                    &Value::Record(BTreeMap::from([
                        ("name".to_owned(), Value::Text("system".to_owned())),
                        ("request".to_owned(), Value::Text(capability.to_owned())),
                    ])),
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (CORE_MATH_TYPE, _, _) => Ok((math_capability(capability, arguments)?, None)),
            (CORE_VALUE_TYPE | CORE_TEXT_TYPE, _, _) => {
                let value = self.manager.value(self.context, object)?;
                Ok((text_capability(capability, arguments, &value)?, None))
            }
            (CORE_OBJECT_STORE_TYPE, "stats" | "health_check", []) => {
                self.manager.health_check()?;
                let stats = self.manager.stats()?;
                Ok((
                    Value::Record(BTreeMap::from([
                        ("healthy".to_owned(), Value::Bool(true)),
                        (
                            "shards".to_owned(),
                            Value::Integer(i64::from(stats.shard_count)),
                        ),
                        (
                            "objects".to_owned(),
                            Value::Integer(count_integer(stats.object_count)?),
                        ),
                        (
                            "active".to_owned(),
                            Value::Integer(count_integer(stats.active_count)?),
                        ),
                        (
                            "tombstoned".to_owned(),
                            Value::Integer(count_integer(stats.tombstoned_count)?),
                        ),
                    ])),
                    None,
                ))
            }
            (CORE_OBJECT_STORE_TYPE, "effects", []) => {
                self.require_local()?;
                Ok((
                    Value::Array(
                        self.manager
                            .query(
                                self.context,
                                &ObjectQuery::new().with_type(CORE_EFFECT_TYPE),
                            )?
                            .into_iter()
                            .map(|header| Value::Text(header.id.to_string()))
                            .collect(),
                    ),
                    None,
                ))
            }
            (CORE_TYPE_REGISTRY_TYPE, "types", []) => Ok((
                Value::Array(
                    self.manager
                        .types()?
                        .iter()
                        .map(type_descriptor_value)
                        .collect(),
                ),
                None,
            )),
            (
                CORE_TYPE_REGISTRY_TYPE,
                "register",
                [
                    Value::Text(name),
                    Value::Text(schema),
                    Value::Text(creation),
                    Value::Array(capabilities),
                ],
            ) => {
                self.require_local()?;
                let schema = match schema.as_str() {
                    "any" => ValueSchema::Any,
                    "text" => ValueSchema::Text,
                    "bytes" => ValueSchema::Bytes,
                    "collection" => ValueSchema::Collection,
                    "record" => ValueSchema::Record,
                    _ => return Err(VmError::TypeError("unknown Type schema")),
                };
                let creation = match creation.as_str() {
                    "public" => CreationPolicy::Public,
                    "provider_only" => CreationPolicy::ProviderOnly,
                    _ => return Err(VmError::TypeError("unknown Type creation policy")),
                };
                let capabilities = capabilities
                    .iter()
                    .map(|value| match value {
                        Value::Text(value) => Ok(value.clone()),
                        _ => Err(VmError::TypeError("Type capabilities must be Text")),
                    })
                    .collect::<Result<BTreeSet<_>, _>>()?;
                let (descriptor, request) = self.manager.prepare_register_type(
                    AccessContext::new(SYSTEM_SUBJECT),
                    name,
                    schema,
                    creation,
                    capabilities,
                )?;
                transaction.create(request);
                Ok((type_descriptor_value(&descriptor), None))
            }
            (CORE_COMPILER_TYPE, "compile", [Value::Text(source)]) => {
                let program =
                    compile(source).map_err(|error| VmError::Provider(error.to_string()))?;
                let request = CreateObject::new(CORE_PROGRAM_TYPE, program.encode()?)
                    .with_parent(current_process);
                let program = request.id;
                transaction.create(request);
                Ok((Value::Text(program.to_string()), None))
            }
            (CORE_COMPILER_TYPE, "validate", [Value::Text(source)]) => {
                Ok((Value::Bool(compile(source).is_ok()), None))
            }
            (CORE_COMPILER_TYPE, "disassemble", [Value::Text(program)]) => {
                let program = program
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Program ObjectId"))?;
                let view = self.manager.read(self.context, program)?;
                if view.header().type_id != CORE_PROGRAM_TYPE {
                    return Err(VmError::TypeError("Object is not a Program"));
                }
                let program = Program::decode(view.state())?;
                Ok((Value::Text(format!("{:?}", program.tokens)), None))
            }
            (CORE_TYPE_REGISTRY_TYPE, "descriptor", [Value::Text(name)]) => Ok((
                type_descriptor_value(&self.manager.type_by_name(name)?),
                None,
            )),
            (CORE_PROVIDER_REGISTRY_TYPE, "providers", []) => {
                self.require_local()?;
                Ok((
                    Value::Array(
                        self.providers
                            .types()?
                            .into_iter()
                            .map(|type_id| Value::Text(type_id.to_string()))
                            .collect(),
                    ),
                    None,
                ))
            }
            (CORE_PROVIDER_REGISTRY_TYPE, "devices", []) => {
                self.require_local()?;
                Ok((
                    Value::Array(
                        self.manager
                            .list(self.context)?
                            .into_iter()
                            .filter(|item| (0x1300..=0x13ff).contains(&item.type_id.as_u128()))
                            .map(|item| Value::Text(item.id.to_string()))
                            .collect(),
                    ),
                    None,
                ))
            }
            (CORE_EFFECT_TYPE, "status", []) => {
                let record =
                    EffectRecord::decode(self.manager.read(self.context, object)?.state())?;
                let status = match record.status {
                    ousject_provider::EffectStatus::Pending => "pending",
                    ousject_provider::EffectStatus::Completed => "completed",
                    ousject_provider::EffectStatus::Failed => "failed",
                };
                Ok((Value::Text(status.to_owned()), None))
            }
            (CORE_EFFECT_TYPE, "result", []) => {
                let record =
                    EffectRecord::decode(self.manager.read(self.context, object)?.state())?;
                Ok((record.result.unwrap_or(Value::Null), None))
            }
            (CORE_SESSION_TYPE, "revoke", []) => {
                transaction.expect(object, header.version).tombstone(object);
                Ok((Value::Null, None))
            }
            (CONSOLE_TYPE, "print" | "println", [_])
            | (CONSOLE_TYPE, "read_line" | "read_secret", []) => {
                Err(VmError::MissingProvider("console"))
            }
            (PROCESS_TYPE, "start" | "resume", []) => {
                self.change_process_status(
                    current_process,
                    current_state,
                    object,
                    ProcessStatus::Running,
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (PROCESS_TYPE, "suspend", []) => {
                self.change_process_status(
                    current_process,
                    current_state,
                    object,
                    ProcessStatus::Suspended,
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (PROCESS_TYPE, "terminate", []) => {
                self.change_process_status(
                    current_process,
                    current_state,
                    object,
                    ProcessStatus::Terminated,
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (PROCESS_TYPE, "wait", []) if object != current_process => {
                Ok((self.wait_for_process(object)?, None))
            }
            (PROCESS_TYPE, "wait", []) => Ok((
                Value::Text(process_status_name(current_state.status).to_owned()),
                None,
            )),
            (PROCESS_TYPE, "bindings", []) => {
                let state = decode_process_state(self.manager.read(self.context, object)?.state())?;
                Ok((
                    Value::Record(
                        state
                            .variables
                            .into_iter()
                            .map(|(name, object)| (name, Value::Text(object.to_string())))
                            .collect(),
                    ),
                    None,
                ))
            }
            (PROGRAM_TYPE, "execute", []) => {
                let mut request =
                    self.prepare_program_execution(object, current_process, BTreeMap::new())?;
                let id = request.id;
                request.links.insert("process".to_owned(), id);
                transaction.create(request);
                Ok((Value::Text(id.to_string()), None))
            }
            (PROGRAM_TYPE, "execute", [Value::Map(bindings) | Value::Record(bindings)]) => {
                let mut variables = BTreeMap::new();
                for (name, value) in bindings {
                    let variable = object_id(value)?;
                    let header = self.manager.inspect(self.context, variable)?;
                    if header.type_id != CORE_VALUE_TYPE {
                        return Err(VmError::TypeError(
                            "Process context entries must point to core.value Objects",
                        ));
                    }
                    variables.insert(name.clone(), variable);
                }
                let mut request =
                    self.prepare_program_execution(object, current_process, variables)?;
                let id = request.id;
                request.links.insert("process".to_owned(), id);
                transaction.create(request);
                Ok((Value::Text(id.to_string()), None))
            }
            (PROGRAM_TYPE, "execute", [Value::Text(entry)]) => {
                let initial = Value::Record(BTreeMap::from([
                    ("entry".to_owned(), Value::Text(entry.clone())),
                    ("start".to_owned(), Value::Bool(true)),
                ]));
                let mut request = self.prepare_process_create(object, &initial, current_process)?;
                let id = request.id;
                request.links.insert("process".to_owned(), id);
                transaction.create(request);
                Ok((Value::Text(id.to_string()), None))
            }
            (CORE_NAMESPACE_TYPE, "resolve", [Value::Text(path)]) => Ok((
                Value::Text(
                    self.manager
                        .resolve(self.context, object, path)?
                        .to_string(),
                ),
                None,
            )),
            (CORE_NAMESPACE_TYPE, "bind", [Value::Text(name), target]) => {
                validate_namespace_name(name)?;
                let target = object_id(target)?;
                self.manager.inspect(self.context, target)?;
                let view = self.manager.read(self.context, object)?;
                if view.links().contains_key(name) {
                    return Err(VmError::TypeError("namespace name already exists"));
                }
                transaction.expect(object, view.header().version).set_link(
                    object,
                    name.clone(),
                    target,
                );
                Ok((Value::Null, None))
            }
            (CORE_NAMESPACE_TYPE, "unbind", [Value::Text(name)]) => {
                validate_namespace_name(name)?;
                let view = self.manager.read(self.context, object)?;
                if !view.links().contains_key(name) {
                    return Err(VmError::MissingKey(name.clone()));
                }
                transaction
                    .expect(object, view.header().version)
                    .remove_link(object, name.clone());
                Ok((Value::Null, None))
            }
            (CORE_CHANNEL_TYPE, "send", [value]) => {
                let view = self.manager.read(self.context, object)?;
                let Value::Array(mut messages) = self.manager.value(self.context, object)? else {
                    return Err(VmError::TypeError("Channel state must be an Array"));
                };
                messages.push(value.clone());
                self.stage_object_value(object, &Value::Array(messages.clone()), transaction)?;
                for (name, waiter) in view
                    .links()
                    .iter()
                    .filter(|(name, _)| name.starts_with("$wait:"))
                {
                    let process_view = self
                        .manager
                        .read(AccessContext::new(SYSTEM_SUBJECT), *waiter)?;
                    if process_view.links().get("$waiting_on") != Some(&object) {
                        transaction
                            .expect(object, view.header().version)
                            .remove_link(object, name.clone());
                        continue;
                    }
                    let mut process_state = decode_process_state(process_view.state())?;
                    if process_state.status == ProcessStatus::Suspended {
                        process_state.status = ProcessStatus::Running;
                        transaction
                            .expect(*waiter, process_view.header().version)
                            .update_state(*waiter, encode_process_state(&process_state)?);
                    }
                    transaction.remove_link(*waiter, "$waiting_on");
                    transaction
                        .expect(object, view.header().version)
                        .remove_link(object, name.clone());
                }
                Ok((
                    Value::Integer(
                        i64::try_from(messages.len())
                            .map_err(|_| VmError::TypeError("Channel is too large"))?,
                    ),
                    None,
                ))
            }
            (CORE_CHANNEL_TYPE, "receive", []) => {
                let Value::Array(mut messages) = self.manager.value(self.context, object)? else {
                    return Err(VmError::TypeError("Channel state must be an Array"));
                };
                if messages.is_empty() {
                    Ok((Value::Null, None))
                } else {
                    let message = messages.remove(0);
                    self.stage_object_value(object, &Value::Array(messages), transaction)?;
                    Ok((message, None))
                }
            }
            (CORE_CHANNEL_TYPE, "wait", []) => {
                let Value::Array(messages) = self.manager.value(self.context, object)? else {
                    return Err(VmError::TypeError("Channel state must be an Array"));
                };
                if messages.is_empty() {
                    let view = self.manager.read(self.context, object)?;
                    transaction.expect(object, view.header().version).set_link(
                        object,
                        format!("$wait:{current_process}"),
                        current_process,
                    );
                    transaction.set_link(current_process, "$waiting_on", object);
                    current_state.status = ProcessStatus::Suspended;
                }
                Ok((Value::Null, None))
            }
            _ => Err(VmError::TypeError("unknown Object capability or arguments")),
        }
    }

    fn stage_object_value(
        &self,
        object: ObjectId,
        value: &Value,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let view = self.manager.read(self.context, object)?;
        let encoded = self
            .manager
            .prepare_replace_value(self.context, object, value)?
            .1;
        transaction
            .expect(object, view.header().version)
            .update_state(object, encoded);
        Ok(())
    }

    fn change_process_status(
        &self,
        current_process: ObjectId,
        current_state: &mut ProcessState,
        target: ObjectId,
        status: ProcessStatus,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let ended_at_unix_ms = matches!(
            status,
            ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
        )
        .then(unix_time_millis);
        if target == current_process {
            current_state.status = status;
            current_state.wake_at_unix_ms = None;
            current_state.ended_at_unix_ms = ended_at_unix_ms;
            return Ok(());
        }
        let view = self.manager.read(self.context, target)?;
        let mut state = decode_process_state(view.state())?;
        state.status = status;
        state.wake_at_unix_ms = None;
        state.ended_at_unix_ms = ended_at_unix_ms;
        transaction
            .expect(target, view.header().version)
            .update_state(target, encode_process_state(&state)?);
        Ok(())
    }

    fn stage_process_subject_access(
        &self,
        process: ObjectId,
        state: &ProcessState,
        subject: SubjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let mut objects = BTreeSet::from([process, state.program]);
        objects.extend(state.variables.values().copied());
        for frame in &state.frames {
            objects.extend(frame.locals.values().copied());
            if let Some(receiver) = frame.receiver {
                objects.insert(receiver);
            }
        }
        for object in objects {
            let header = self.manager.inspect(self.context, object)?;
            transaction.expect(object, header.version);
            for capability in [
                Capability::Inspect,
                Capability::ViewValue,
                Capability::ReplaceValue,
                Capability::CreateChild,
                Capability::Invoke,
                Capability::Link,
                Capability::Reparent,
                Capability::Retire,
            ] {
                transaction.grant(object, subject, capability);
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn enter_function(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        program: &Program,
        name: &str,
        arguments: u32,
    ) -> Result<Option<String>, VmError> {
        let args = pop_call_arguments(&mut state.stack, arguments)?;
        let (entry, parameters) = find_function(program, name, args.len())?;
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, version);
        let locals = self.stage_arguments(process, &parameters, args, &mut transaction)?;
        state.frames.push(CallFrame {
            return_position: next,
            stack_base: u32::try_from(state.stack.len())
                .map_err(|_| invalid_state("stack is too large"))?,
            locals,
            receiver: None,
            class: None,
        });
        state.token_position = entry;
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn enter_method(
        &self,
        process: ObjectId,
        _version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        program: &Program,
        receiver: &Value,
        method: &str,
        args: Vec<Value>,
        start_class: Option<&str>,
        mut transaction: Transaction,
    ) -> Result<Option<String>, VmError> {
        let receiver = object_id(receiver)?;
        let class = instance_class(&self.manager.value(self.context, receiver)?)?;
        let lookup = start_class.unwrap_or(&class);
        let definition = find_method(program, lookup, method, args.len())?;
        let internal = state.frames.last().is_some_and(|frame| {
            frame.receiver == Some(receiver) && frame.class.as_deref() == Some(&definition.class)
        });
        if !definition.public && !internal {
            return Err(VmError::TypeError("private method is not visible"));
        }
        let locals =
            self.stage_arguments(process, &definition.parameters, args, &mut transaction)?;
        state.frames.push(CallFrame {
            return_position: next,
            stack_base: u32::try_from(state.stack.len())
                .map_err(|_| invalid_state("stack is too large"))?,
            locals,
            receiver: Some(receiver),
            class: Some(definition.class),
        });
        state.token_position = definition.entry;
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(None)
    }

    #[allow(clippy::too_many_arguments)]
    fn step_super_call(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        next: u32,
        program: &Program,
        method: &str,
        arguments: u32,
    ) -> Result<Option<String>, VmError> {
        let args = pop_call_arguments(&mut state.stack, arguments)?;
        let receiver = state.stack.pop().ok_or(VmError::StackUnderflow)?;
        let class = state
            .frames
            .last()
            .and_then(|frame| frame.class.as_deref())
            .ok_or(VmError::TypeError("super outside method"))?;
        let parent = class_definition(program, class)?
            .parent
            .ok_or(VmError::TypeError("class has no parent"))?;
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, version);
        self.enter_method(
            process,
            version,
            state,
            next,
            program,
            &receiver,
            method,
            args,
            Some(&parent),
            transaction,
        )
    }

    fn stage_arguments(
        &self,
        process: ObjectId,
        parameters: &[String],
        arguments: Vec<Value>,
        transaction: &mut Transaction,
    ) -> Result<BTreeMap<String, ObjectId>, VmError> {
        let mut locals = BTreeMap::new();
        for (parameter, value) in parameters.iter().zip(arguments) {
            let mut request = self
                .manager
                .prepare_create(CreateSpec::new("core.value", value).with_parent(process))?;
            while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                request.id = ObjectId::new();
            }
            locals.insert(parameter.clone(), request.id);
            transaction.create(request);
        }
        Ok(locals)
    }

    fn prepare_object_create(
        &self,
        program_id: ObjectId,
        type_name: &str,
        initial: &Value,
        parent: ObjectId,
    ) -> Result<CreateObject, VmError> {
        if type_name == "core.process" {
            return self.prepare_process_create(program_id, initial, parent);
        }
        match self
            .manager
            .prepare_create(CreateSpec::new(type_name, initial.clone()).with_parent(parent))
        {
            Ok(request) => Ok(request),
            Err(OmsError::TypeCreationDenied(type_id)) => {
                let provider = self.providers.get(type_id).map_err(VmError::from)?;
                if !provider.user_creatable() {
                    return Err(OmsError::TypeCreationDenied(type_id).into());
                }
                let descriptor = self.manager.type_by_id(type_id)?;
                let state = provider.create(initial).map_err(VmError::from)?.encode()?;
                let mut request = CreateObject::new(type_id, state).with_parent(parent);
                request.capabilities = descriptor.capabilities;
                Ok(request)
            }
            Err(OmsError::UnknownTypeName(_)) => {
                let program_view = self.manager.read(self.context, program_id)?;
                let program = Program::decode(program_view.state())?;
                let mut fields = collect_class_fields(&program, type_name)?;
                match initial {
                    Value::Map(values) | Value::Record(values) => {
                        for (name, value) in values {
                            if !fields.contains_key(name) {
                                return Err(VmError::MissingKey(name.clone()));
                            }
                            fields.insert(name.clone(), value.clone());
                        }
                    }
                    Value::Null => {}
                    _ => {
                        return Err(VmError::TypeError(
                            "class initial value must be a Map, Record or null",
                        ));
                    }
                }
                fields.insert("$class".to_owned(), Value::Text(type_name.to_owned()));
                Ok(
                    CreateObject::new(INSTANCE_TYPE, Value::Record(fields).encode()?)
                        .with_parent(parent),
                )
            }
            Err(error) => Err(error.into()),
        }
    }

    fn prepare_process_create(
        &self,
        program_id: ObjectId,
        initial: &Value,
        parent: ObjectId,
    ) -> Result<CreateObject, VmError> {
        let (Value::Map(options) | Value::Record(options)) = initial else {
            return Err(VmError::TypeError(
                "Process creation requires { entry, start?, links? }",
            ));
        };
        let Some(Value::Text(entry_name)) = options.get("entry") else {
            return Err(VmError::TypeError("Process entry must be a function name"));
        };
        let start = match options.get("start") {
            None | Some(Value::Bool(false)) => false,
            Some(Value::Bool(true)) => true,
            Some(_) => return Err(VmError::TypeError("Process start must be Boolean")),
        };
        for key in options.keys() {
            if !matches!(key.as_str(), "entry" | "start" | "links") {
                return Err(VmError::MissingKey(key.clone()));
            }
        }
        let program = Program::decode(self.manager.read(self.context, program_id)?.state())?;
        let (entry, parameters) = find_function(&program, entry_name, 0)?;
        debug_assert!(parameters.is_empty());
        let halt = program
            .tokens
            .iter()
            .position(|token| matches!(token, Token::Halt))
            .and_then(|position| u32::try_from(position).ok())
            .ok_or(VmError::TypeError("Program has no halt token"))?;
        let state = ProcessState {
            program: program_id,
            subject: self.context.subject,
            token_position: entry,
            stack: Vec::new(),
            variables: BTreeMap::new(),
            status: if start {
                ProcessStatus::Running
            } else {
                ProcessStatus::Suspended
            },
            result: None,
            error: None,
            wake_at_unix_ms: None,
            ended_at_unix_ms: None,
            frames: vec![CallFrame {
                return_position: halt,
                stack_base: 0,
                locals: BTreeMap::new(),
                receiver: None,
                class: None,
            }],
            handlers: Vec::new(),
        };
        let mut request = CreateObject::new(PROCESS_TYPE, encode_process_state(&state)?)
            .with_parent(parent)
            .with_link("program", program_id);
        if let Some(console) = self.console_provider {
            request = request.with_link("console", console);
        }
        for (name, service) in &self.kernel_services {
            request = request.with_link(name.clone(), *service);
        }
        if let Some(links) = options.get("links") {
            let (Value::Map(links) | Value::Record(links)) = links else {
                return Err(VmError::TypeError("Process links must be a Map"));
            };
            for (name, target) in links {
                request = request.with_link(name, object_id(target)?);
            }
        }
        Ok(request)
    }

    fn prepare_program_execution(
        &self,
        program: ObjectId,
        parent: ObjectId,
        variables: BTreeMap<String, ObjectId>,
    ) -> Result<CreateObject, VmError> {
        Program::decode(self.manager.read(self.context, program)?.state())?;
        let state = ProcessState {
            program,
            subject: self.context.subject,
            token_position: 0,
            stack: Vec::new(),
            variables,
            status: ProcessStatus::Running,
            result: None,
            error: None,
            wake_at_unix_ms: None,
            ended_at_unix_ms: None,
            frames: Vec::new(),
            handlers: Vec::new(),
        };
        let mut request = CreateObject::new(PROCESS_TYPE, encode_process_state(&state)?)
            .with_parent(parent)
            .with_link("program", program);
        if let Some(console) = self.console_provider {
            request = request.with_link("console", console);
        }
        for (name, service) in &self.kernel_services {
            request = request.with_link(name.clone(), *service);
        }
        Ok(request)
    }

    fn get_field(
        &self,
        state: &ProcessState,
        receiver: &Value,
        field: &str,
        program: &Program,
    ) -> Result<Value, VmError> {
        if let Value::Map(fields) | Value::Record(fields) = receiver {
            return fields
                .get(field)
                .cloned()
                .ok_or_else(|| VmError::MissingKey(field.to_owned()));
        }

        let object = object_id(receiver)?;
        let view = self.manager.read(self.context, object)?;
        if let Ok(value) = Value::decode(view.state()) {
            Self::check_field_visibility(state, object, field, program, &value)?;
            if let Value::Map(fields) | Value::Record(fields) = &value {
                if let Some(value) = fields.get(field) {
                    return Ok(value.clone());
                }
            }
        }

        if let Some(value) = self.object_property(object, &view, field, program)? {
            return Ok(value);
        }

        if view.header().type_id == PROCESS_TYPE {
            let process = decode_process_state(view.state())?;
            if let Some(variable) = process.variables.get(field) {
                return self
                    .manager
                    .value(self.context, *variable)
                    .map_err(Into::into);
            }
        }

        Err(VmError::MissingKey(field.to_owned()))
    }

    #[allow(clippy::too_many_lines)]
    fn object_property(
        &self,
        object: ObjectId,
        view: &oms_runtime::ObjectView,
        field: &str,
        program: &Program,
    ) -> Result<Option<Value>, VmError> {
        let header = view.header();
        let descriptor = self.manager.type_by_id(header.type_id)?;
        let type_name = if header.type_id == INSTANCE_TYPE {
            instance_class(&self.manager.value(self.context, object)?)?
        } else {
            descriptor.name.clone()
        };
        let value = match field {
            "id" => Value::Text(object.to_string()),
            "type" => Value::Text(type_name.clone()),
            "parent" => header
                .parent_id
                .map_or(Value::Null, |id| Value::Text(id.to_string())),
            "status" => {
                if header.type_id == PROCESS_TYPE {
                    Value::Text(
                        process_status_name(decode_process_state(view.state())?.status).to_owned(),
                    )
                } else {
                    Value::Text(format!("{:?}", header.lifecycle))
                }
            }
            "result" if header.type_id == PROCESS_TYPE => decode_process_state(view.state())?
                .result
                .unwrap_or(Value::Null),
            "error" if header.type_id == PROCESS_TYPE => decode_process_state(view.state())?
                .error
                .unwrap_or(Value::Null),
            "version" => Value::Text(header.version.get().to_string()),
            "owner" => Value::Text(view.owner().to_string()),
            "permissions" => {
                self.manager
                    .require_capability(self.context, object, Capability::ManagePolicy)?;
                Value::Record(BTreeMap::from([
                    ("owner".to_owned(), Value::Text(view.owner().to_string())),
                    (
                        "grants".to_owned(),
                        Value::Record(
                            view.grants()
                                .iter()
                                .map(|(subject, capabilities)| {
                                    (
                                        subject.to_string(),
                                        Value::Array(
                                            capabilities
                                                .iter()
                                                .map(|capability| {
                                                    Value::Text(format!("{capability:?}"))
                                                })
                                                .collect(),
                                        ),
                                    )
                                })
                                .collect(),
                        ),
                    ),
                ]))
            }
            "inspect" => Value::Record(BTreeMap::from([
                ("id".to_owned(), Value::Text(object.to_string())),
                ("type".to_owned(), Value::Text(type_name)),
                (
                    "parent".to_owned(),
                    header
                        .parent_id
                        .map_or(Value::Null, |id| Value::Text(id.to_string())),
                ),
                (
                    "version".to_owned(),
                    Value::Text(header.version.get().to_string()),
                ),
                (
                    "status".to_owned(),
                    if header.type_id == PROCESS_TYPE {
                        Value::Text(
                            process_status_name(decode_process_state(view.state())?.status)
                                .to_owned(),
                        )
                    } else {
                        Value::Text(format!("{:?}", header.lifecycle))
                    },
                ),
            ])),
            "capabilities" => {
                let mut capabilities: Vec<Value> = view
                    .capabilities()
                    .iter()
                    .map(|capability| Value::Text(format!("{capability:?}")))
                    .collect();
                capabilities.extend(
                    descriptor
                        .domain_capabilities
                        .iter()
                        .cloned()
                        .map(Value::Text),
                );
                if header.type_id == INSTANCE_TYPE {
                    for method in public_methods(program, &type_name)? {
                        let value = Value::Text(method);
                        if !capabilities.contains(&value) {
                            capabilities.push(value);
                        }
                    }
                }
                Value::Array(capabilities)
            }
            "value" => object_essential_value(view)?,
            "children" => Value::Array(
                view.children()
                    .iter()
                    .map(|id| Value::Text(id.to_string()))
                    .collect(),
            ),
            "links" => Value::Map(
                view.links()
                    .iter()
                    .map(|(name, target)| (name.clone(), Value::Text(target.to_string())))
                    .collect(),
            ),
            "variables" if header.type_id == PROCESS_TYPE => {
                let process = decode_process_state(view.state())?;
                let mut variables = BTreeMap::new();
                for (name, object) in process.variables {
                    variables.insert(name, self.manager.value(self.context, object)?);
                }
                Value::Record(variables)
            }
            "program" if header.type_id == PROCESS_TYPE => {
                Value::Text(decode_process_state(view.state())?.program.to_string())
            }
            "user" | "subject" if header.type_id == PROCESS_TYPE => {
                Value::Text(decode_process_state(view.state())?.subject.to_string())
            }
            "position" if header.type_id == PROCESS_TYPE => Value::Integer(i64::from(
                decode_process_state(view.state())?.token_position,
            )),
            _ => return Ok(None),
        };
        Ok(Some(value))
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_field(
        &self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        state: &ProcessState,
        receiver: &Value,
        field: &str,
        value: Value,
        program: &Program,
    ) -> Result<(), VmError> {
        let object = object_id(receiver)?;
        let view = self.manager.read(self.context, object)?;
        let current = self.manager.value(self.context, object)?;
        Self::check_field_visibility(state, object, field, program, &current)?;
        let (Value::Map(mut fields) | Value::Record(mut fields)) = current else {
            return Err(VmError::TypeError("Object value has no fields"));
        };
        if !fields.contains_key(field) {
            return Err(VmError::MissingKey(field.to_owned()));
        }
        fields.insert(field.to_owned(), value);
        let replacement = Value::Record(fields).encode()?;
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, process_version)
            .expect(object, view.header().version)
            .update_state(object, replacement)
            .update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(())
    }

    fn check_field_visibility(
        state: &ProcessState,
        object: ObjectId,
        field: &str,
        program: &Program,
        value: &Value,
    ) -> Result<(), VmError> {
        let Ok(class) = instance_class(value) else {
            return Ok(());
        };
        if let Some(owner) = private_field_owner(program, &class, field)? {
            let internal = state.frames.last().is_some_and(|frame| {
                frame.receiver == Some(object) && frame.class.as_deref() == Some(&owner)
            });
            if !internal {
                return Err(VmError::TypeError("private field is not visible"));
            }
        }
        Ok(())
    }

    fn query_objects(
        &self,
        program_id: ObjectId,
        type_name: &str,
        capability: Option<&str>,
    ) -> Result<Value, VmError> {
        let (type_id, class_filter) = match self.manager.type_by_name(type_name) {
            Ok(descriptor) => (descriptor.id, None),
            Err(OmsError::UnknownTypeName(_)) => {
                let program =
                    Program::decode(self.manager.read(self.context, program_id)?.state())?;
                class_definition(&program, type_name)?;
                (INSTANCE_TYPE, Some(type_name))
            }
            Err(error) => return Err(error.into()),
        };
        let mut query = ObjectQuery::new().with_type(type_id);
        if class_filter.is_none() {
            if let Some(capability) = capability {
                query = query.with_domain_capability(capability);
            }
        } else if let (Some(class), Some(capability)) = (class_filter, capability) {
            let program = Program::decode(self.manager.read(self.context, program_id)?.state())?;
            if !public_methods(&program, class)?
                .iter()
                .any(|method| method == capability)
            {
                return Ok(Value::Array(Vec::new()));
            }
        }
        let headers = self.manager.query(self.context, &query)?;
        let mut objects = Vec::new();
        for header in headers {
            if let Some(class) = class_filter {
                let value = self.manager.value(self.context, header.id)?;
                if instance_class(&value)?.as_str() != class {
                    continue;
                }
            }
            objects.push(Value::Text(header.id.to_string()));
        }
        Ok(Value::Array(objects))
    }

    #[allow(clippy::too_many_lines)]
    fn object_operation(
        &self,
        program_id: ObjectId,
        method: &str,
        args: &[Value],
        transaction: &mut Transaction,
    ) -> Result<Value, VmError> {
        let (identity, args) = args
            .split_first()
            .ok_or(VmError::TypeError("Object operation requires an ID"))?;
        let object = object_id(identity)?;
        let view = self.manager.read(self.context, object)?;
        let descriptor = self.manager.type_by_id(view.header().type_id)?;
        let no_args = args.is_empty();
        match method {
            "id" if no_args => Ok(Value::Text(object.to_string())),
            "type" if no_args => {
                if view.header().type_id == INSTANCE_TYPE {
                    Ok(Value::Text(instance_class(
                        &self.manager.value(self.context, object)?,
                    )?))
                } else {
                    Ok(Value::Text(descriptor.name.clone()))
                }
            }
            "parent" if no_args => Ok(view
                .header()
                .parent_id
                .map_or(Value::Null, |id| Value::Text(id.to_string()))),
            "status" if no_args => {
                if view.header().type_id == PROCESS_TYPE {
                    Ok(Value::Text(
                        process_status_name(decode_process_state(view.state())?.status).to_owned(),
                    ))
                } else {
                    Ok(Value::Text(format!("{:?}", view.header().lifecycle)))
                }
            }
            "inspect" if no_args => {
                let header = view.header();
                let type_name = if header.type_id == INSTANCE_TYPE {
                    instance_class(&self.manager.value(self.context, object)?)?
                } else {
                    descriptor.name.clone()
                };
                Ok(Value::Record(BTreeMap::from([
                    ("id".to_owned(), Value::Text(object.to_string())),
                    ("type".to_owned(), Value::Text(type_name)),
                    (
                        "parent".to_owned(),
                        header
                            .parent_id
                            .map_or(Value::Null, |id| Value::Text(id.to_string())),
                    ),
                    (
                        "version".to_owned(),
                        Value::Text(header.version.get().to_string()),
                    ),
                    (
                        "status".to_owned(),
                        if header.type_id == PROCESS_TYPE {
                            Value::Text(
                                process_status_name(decode_process_state(view.state())?.status)
                                    .to_owned(),
                            )
                        } else {
                            Value::Text(format!("{:?}", header.lifecycle))
                        },
                    ),
                ])))
            }
            "capabilities" if no_args => {
                let mut capabilities: Vec<Value> = view
                    .capabilities()
                    .iter()
                    .map(|capability| Value::Text(format!("{capability:?}")))
                    .collect();
                capabilities.extend(
                    descriptor
                        .domain_capabilities
                        .iter()
                        .cloned()
                        .map(Value::Text),
                );
                if view.header().type_id == INSTANCE_TYPE {
                    let class = instance_class(&self.manager.value(self.context, object)?)?;
                    let program =
                        Program::decode(self.manager.read(self.context, program_id)?.state())?;
                    for method in public_methods(&program, &class)? {
                        let value = Value::Text(method);
                        if !capabilities.contains(&value) {
                            capabilities.push(value);
                        }
                    }
                }
                Ok(Value::Array(capabilities))
            }
            "value" if no_args => self.manager.value(self.context, object).map_err(Into::into),
            "children" if no_args => Ok(Value::Array(
                view.children()
                    .iter()
                    .map(|id| Value::Text(id.to_string()))
                    .collect(),
            )),
            "links" if no_args => Ok(Value::Map(
                view.links()
                    .iter()
                    .map(|(name, target)| (name.clone(), Value::Text(target.to_string())))
                    .collect(),
            )),
            "replace" if args.len() == 1 => {
                if view.header().type_id == CORE_USER_TYPE
                    && is_local_user_value(&self.manager.value(self.context, object)?)
                {
                    return Err(VmError::TypeError(
                        "the reserved local User can only change through authentication",
                    ));
                }
                let (version, encoded) =
                    self.manager
                        .prepare_replace_value(self.context, object, &args[0])?;
                transaction
                    .expect(object, version)
                    .update_state(object, encoded);
                Ok(Value::Null)
            }
            "link" if args.len() == 2 => {
                let Value::Text(name) = &args[0] else {
                    return Err(VmError::TypeError("link name must be text"));
                };
                let target = object_id(&args[1])?;
                transaction.expect(object, view.header().version).set_link(
                    object,
                    name.clone(),
                    target,
                );
                Ok(Value::Null)
            }
            "unlink" if args.len() == 1 => {
                let Value::Text(name) = &args[0] else {
                    return Err(VmError::TypeError("link name must be text"));
                };
                transaction
                    .expect(object, view.header().version)
                    .remove_link(object, name.clone());
                Ok(Value::Null)
            }
            "grant" | "revoke" if args.len() == 2 => {
                let Value::Text(subject) = &args[0] else {
                    return Err(VmError::TypeError("subject must be hexadecimal text"));
                };
                let subject = subject
                    .parse::<SubjectId>()
                    .map_err(|_| VmError::TypeError("invalid hexadecimal SubjectId"))?;
                let Value::Text(capability) = &args[1] else {
                    return Err(VmError::TypeError("capability name must be text"));
                };
                let capability = capability_from_name(capability)?;
                transaction.expect(object, view.header().version);
                if method == "grant" {
                    transaction.grant(object, subject, capability);
                } else {
                    transaction.revoke(object, subject, capability);
                }
                Ok(Value::Null)
            }
            _ => Err(VmError::TypeError("invalid Object method or arguments")),
        }
    }

    fn variable_from_state(&self, state: &ProcessState, name: &str) -> Result<Value, VmError> {
        let object = binding_id(state, name)?;
        let view = self.manager.read(self.context, object)?;
        decode_value_state(view.state())
    }

    fn commit_process(
        &self,
        process: ObjectId,
        version: oms_types::ObjectVersion,
        state: &ProcessState,
    ) -> Result<(), VmError> {
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(process, version)
            .update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(())
    }

    fn commit_store(
        &self,
        process: ObjectId,
        process_version: oms_types::ObjectVersion,
        state: &mut ProcessState,
        name: String,
        value: &Value,
    ) -> Result<(), VmError> {
        let mut transaction = self.manager.begin(self.context);
        transaction.expect(process, process_version);
        let existing = state
            .frames
            .last()
            .and_then(|frame| frame.locals.get(&name))
            .or_else(|| state.variables.get(&name))
            .copied();
        if let Some(variable) = existing {
            let variable_view = self.manager.read(self.context, variable)?;
            let encoded = self
                .manager
                .prepare_replace_value(self.context, variable, value)?
                .1;
            transaction
                .expect(variable, variable_view.header().version)
                .update_state(variable, encoded);
        } else {
            let mut request = self.manager.prepare_create(
                CreateSpec::new("core.value", value.clone()).with_parent(process),
            )?;
            while self.manager.shard_for(request.id) != self.manager.shard_for(process) {
                request.id = ObjectId::new();
            }
            request = request.with_parent(process);
            bind_name(state, name, request.id);
            transaction.create(request);
        }
        transaction.update_state(process, encode_process_state(state)?);
        self.manager.commit(transaction)?;
        Ok(())
    }
}

fn pop_call_arguments(stack: &mut Vec<Value>, count: u32) -> Result<Vec<Value>, VmError> {
    let count = usize::try_from(count).map_err(|_| VmError::StackUnderflow)?;
    let start = stack
        .len()
        .checked_sub(count)
        .ok_or(VmError::StackUnderflow)?;
    Ok(stack.split_off(start))
}

#[derive(Debug, Clone)]
struct ClassDefinition {
    parent: Option<String>,
    fields: BTreeMap<String, Value>,
    private_fields: Vec<String>,
}

#[derive(Debug, Clone)]
struct MethodDefinition {
    class: String,
    parameters: Vec<String>,
    public: bool,
    entry: u32,
}

fn binding_id(state: &ProcessState, name: &str) -> Result<ObjectId, VmError> {
    if matches!(name, "this" | "super") {
        return state
            .frames
            .last()
            .and_then(|frame| frame.receiver)
            .ok_or(VmError::TypeError("this or super outside method"));
    }
    state
        .frames
        .last()
        .and_then(|frame| frame.locals.get(name))
        .or_else(|| state.variables.get(name))
        .copied()
        .ok_or_else(|| VmError::UndefinedVariable(name.to_owned()))
}

fn bind_name(state: &mut ProcessState, name: String, object: ObjectId) {
    if let Some(frame) = state.frames.last_mut() {
        frame.locals.insert(name, object);
    } else {
        state.variables.insert(name, object);
    }
}

fn remove_retired_bindings(state: &mut ProcessState, retired: &BTreeSet<ObjectId>) -> bool {
    let mut changed = false;
    state.variables.retain(|_, object| {
        let keep = !retired.contains(object);
        changed |= !keep;
        keep
    });
    for frame in &mut state.frames {
        frame.locals.retain(|_, object| {
            let keep = !retired.contains(object);
            changed |= !keep;
            keep
        });
    }
    changed
}

fn find_function(
    program: &Program,
    name: &str,
    arity: usize,
) -> Result<(u32, Vec<String>), VmError> {
    let found = program
        .tokens
        .iter()
        .enumerate()
        .find_map(|(position, token)| match token {
            Token::DefineFunction {
                name: candidate,
                parameters,
                ..
            } if candidate == name && parameters.len() == arity => Some((position, parameters)),
            _ => None,
        });
    let (position, parameters) =
        found.ok_or(VmError::TypeError("unknown function or argument count"))?;
    let entry = u32::try_from(position + 1)
        .map_err(|_| VmError::TypeError("function position is too large"))?;
    Ok((entry, parameters.clone()))
}

fn class_definition(program: &Program, name: &str) -> Result<ClassDefinition, VmError> {
    program
        .tokens
        .iter()
        .find_map(|token| match token {
            Token::DefineClass {
                name: candidate,
                parent,
                fields,
                private_fields,
                ..
            } if candidate == name => Some(ClassDefinition {
                parent: parent.clone(),
                fields: fields.clone(),
                private_fields: private_fields.clone(),
            }),
            _ => None,
        })
        .ok_or(VmError::TypeError("unknown class"))
}

fn collect_class_fields(program: &Program, name: &str) -> Result<BTreeMap<String, Value>, VmError> {
    fn collect(
        program: &Program,
        name: &str,
        depth: usize,
        output: &mut BTreeMap<String, Value>,
    ) -> Result<(), VmError> {
        if depth > 64 {
            return Err(VmError::TypeError("class inheritance is too deep"));
        }
        let definition = class_definition(program, name)?;
        if let Some(parent) = definition.parent {
            collect(program, &parent, depth + 1, output)?;
        }
        output.extend(definition.fields);
        Ok(())
    }
    let mut fields = BTreeMap::new();
    collect(program, name, 0, &mut fields)?;
    Ok(fields)
}

fn find_method(
    program: &Program,
    class: &str,
    method: &str,
    arity: usize,
) -> Result<MethodDefinition, VmError> {
    let mut current = Some(class.to_owned());
    for _ in 0..=64 {
        let class_name = current
            .take()
            .ok_or(VmError::TypeError("unknown method or argument count"))?;
        if let Some((position, parameters, public)) =
            program
                .tokens
                .iter()
                .enumerate()
                .find_map(|(position, token)| match token {
                    Token::DefineMethod {
                        class,
                        name,
                        parameters,
                        public,
                        ..
                    } if class == &class_name && name == method && parameters.len() == arity => {
                        Some((position, parameters.clone(), *public))
                    }
                    _ => None,
                })
        {
            return Ok(MethodDefinition {
                class: class_name,
                parameters,
                public,
                entry: u32::try_from(position + 1)
                    .map_err(|_| VmError::TypeError("method position is too large"))?,
            });
        }
        current = class_definition(program, &class_name)?.parent;
    }
    Err(VmError::TypeError("class inheritance is too deep"))
}

fn public_methods(program: &Program, class: &str) -> Result<Vec<String>, VmError> {
    let mut current = Some(class.to_owned());
    let mut seen = BTreeSet::new();
    let mut methods = Vec::new();
    for _ in 0..=64 {
        let Some(class_name) = current.take() else {
            return Ok(methods);
        };
        for token in &program.tokens {
            if let Token::DefineMethod {
                class,
                name,
                public,
                ..
            } = token
            {
                if class == &class_name && seen.insert(name.clone()) && *public {
                    methods.push(name.clone());
                }
            }
        }
        current = class_definition(program, &class_name)?.parent;
    }
    Err(VmError::TypeError("class inheritance is too deep"))
}

fn private_field_owner(
    program: &Program,
    class: &str,
    field: &str,
) -> Result<Option<String>, VmError> {
    let mut current = Some(class.to_owned());
    for _ in 0..=64 {
        let Some(class_name) = current.take() else {
            return Ok(None);
        };
        let definition = class_definition(program, &class_name)?;
        if definition.private_fields.iter().any(|name| name == field) {
            return Ok(Some(class_name));
        }
        current = definition.parent;
    }
    Err(VmError::TypeError("class inheritance is too deep"))
}

fn instance_class(value: &Value) -> Result<String, VmError> {
    let Value::Record(fields) = value else {
        return Err(VmError::TypeError("invalid class instance state"));
    };
    match fields.get("$class") {
        Some(Value::Text(class)) => Ok(class.clone()),
        _ => Err(VmError::TypeError("class instance has no class")),
    }
}

fn object_id(value: &Value) -> Result<ObjectId, VmError> {
    match value {
        Value::Text(id) => id
            .parse()
            .map_err(|_| VmError::TypeError("invalid hexadecimal ObjectId")),
        _ => Err(VmError::TypeError("expected hexadecimal ObjectId text")),
    }
}

fn validate_namespace_name(name: &str) -> Result<(), VmError> {
    if name.is_empty() || name == "." || name == ".." || name.contains('/') || name.contains('\0') {
        Err(VmError::TypeError("invalid Namespace name"))
    } else {
        Ok(())
    }
}

fn is_base_object_operation(method: &str) -> bool {
    matches!(
        method,
        "replace" | "link" | "unlink" | "grant" | "revoke" | "retire"
    )
}

fn capability_from_name(value: &str) -> Result<Capability, VmError> {
    match value {
        "view_value" => Ok(Capability::ViewValue),
        "replace_value" => Ok(Capability::ReplaceValue),
        "create_child" => Ok(Capability::CreateChild),
        "invoke" => Ok(Capability::Invoke),
        "link" => Ok(Capability::Link),
        "reparent" => Ok(Capability::Reparent),
        "retire" => Ok(Capability::Retire),
        "inspect" => Ok(Capability::Inspect),
        "manage_policy" => Ok(Capability::ManagePolicy),
        _ => Err(VmError::TypeError("unknown capability name")),
    }
}

const fn process_status_name(status: ProcessStatus) -> &'static str {
    match status {
        ProcessStatus::Running => "running",
        ProcessStatus::Suspended => "suspended",
        ProcessStatus::Halted => "halted",
        ProcessStatus::Terminated => "terminated",
        ProcessStatus::Failed => "failed",
    }
}

fn unix_time_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn count_integer(value: usize) -> Result<i64, VmError> {
    i64::try_from(value).map_err(|_| VmError::TypeError("count exceeds Praxis Integer"))
}

fn type_descriptor_value(descriptor: &TypeDescriptor) -> Value {
    Value::Record(BTreeMap::from([
        ("id".to_owned(), Value::Text(descriptor.id.to_string())),
        ("name".to_owned(), Value::Text(descriptor.name.clone())),
        (
            "schema".to_owned(),
            Value::Text(format!("{:?}", descriptor.schema).to_lowercase()),
        ),
        (
            "creation".to_owned(),
            Value::Text(format!("{:?}", descriptor.creation).to_lowercase()),
        ),
        (
            "capabilities".to_owned(),
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
}

fn user_identity_value(identity: &UserIdentity) -> Value {
    Value::Record(BTreeMap::from([
        (
            "object".to_owned(),
            Value::Text(identity.object.to_string()),
        ),
        (
            "subject".to_owned(),
            Value::Text(identity.subject.to_string()),
        ),
        ("name".to_owned(), Value::Text(identity.name.clone())),
    ]))
}

fn is_local_user_value(value: &Value) -> bool {
    matches!(
        value,
        Value::Record(fields)
            if fields.get("name") == Some(&Value::Text(LOCAL_USER_NAME.to_owned()))
                && fields.get("subject")
                    == Some(&Value::Text(SYSTEM_SUBJECT.to_string()))
    )
}

pub struct CooperativeScheduler<'a> {
    vm: &'a VirtualMachine,
    queue: VecDeque<ObjectId>,
}

impl<'a> CooperativeScheduler<'a> {
    #[must_use]
    pub const fn new(vm: &'a VirtualMachine) -> Self {
        Self {
            vm,
            queue: VecDeque::new(),
        }
    }

    pub fn enqueue(&mut self, process: ObjectId) {
        if !self.queue.contains(&process) {
            self.queue.push_back(process);
        }
    }

    /// Runs queued Processes cooperatively, one Token per scheduling turn.
    ///
    /// # Errors
    ///
    /// Returns a VM error or [`VmError::StepLimitExceeded`] if not all queued
    /// Processes halt within the total step budget.
    pub fn run(&mut self, step_limit: u64) -> Result<ScheduleReport, VmError> {
        let mut total_steps = 0;
        let mut reports = BTreeMap::<ObjectId, RunReport>::new();
        let mut sleepers = BTreeSet::new();
        loop {
            if self.queue.is_empty() && !sleepers.is_empty() {
                let mut next_wake = None;
                let candidates = sleepers.iter().copied().collect::<Vec<_>>();
                for process in candidates {
                    if self.vm.wake_due_timer(process)? {
                        sleepers.remove(&process);
                        self.enqueue(process);
                    } else {
                        let state = self.vm.process_state(process)?;
                        if state.status == ProcessStatus::Running {
                            sleepers.remove(&process);
                            self.enqueue(process);
                        } else if state.wake_at_unix_ms.is_none() {
                            sleepers.remove(&process);
                            if state.status == ProcessStatus::Running {
                                self.enqueue(process);
                            }
                        } else if let Some(delay) = self.vm.time_until_wake(process)? {
                            next_wake =
                                Some(next_wake.map_or(delay, |current: std::time::Duration| {
                                    current.min(delay)
                                }));
                        }
                    }
                }
                if self.queue.is_empty() {
                    if let Some(delay) = next_wake {
                        std::thread::sleep(delay);
                        continue;
                    }
                    if sleepers.is_empty() {
                        break;
                    }
                }
            }
            let Some(process) = self.queue.pop_front() else {
                break;
            };
            let state = self.vm.process_state(process)?;
            let report = reports.entry(process).or_insert_with(|| RunReport {
                process,
                steps: 0,
                status: state.status,
                output: Vec::new(),
            });
            if state.status != ProcessStatus::Running {
                report.status = state.status;
                continue;
            }
            if total_steps >= step_limit {
                return Err(VmError::StepLimitExceeded(step_limit));
            }
            let executor = self.vm.for_subject(state.subject);
            if let Some(line) = executor.step(process, state)? {
                report.output.push(line);
            }
            report.steps += 1;
            total_steps += 1;
            let next_state = self.vm.process_state(process)?;
            report.status = next_state.status;
            if next_state.status == ProcessStatus::Running {
                self.queue.push_back(process);
            } else if next_state.status == ProcessStatus::Suspended
                && next_state.wake_at_unix_ms.is_some()
            {
                sleepers.insert(process);
            }
        }
        Ok(ScheduleReport {
            total_steps,
            processes: reports.into_values().collect(),
        })
    }
}

fn is_catchable(error: &VmError) -> bool {
    match error {
        VmError::StackUnderflow
        | VmError::UndefinedVariable(_)
        | VmError::TypeError(_)
        | VmError::DivisionByZero
        | VmError::IndexOutOfBounds
        | VmError::MissingKey(_)
        | VmError::MissingProvider(_)
        | VmError::Provider(_) => true,
        VmError::Oms(error) => matches!(
            error,
            OmsError::NotFound(_)
                | OmsError::Denied { .. }
                | OmsError::InvalidLifecycle { .. }
                | OmsError::InvalidOperation(_)
                | OmsError::UnknownTypeName(_)
                | OmsError::UnknownType(_)
                | OmsError::TypeCreationDenied(_)
                | OmsError::ValueSchemaMismatch { .. }
                | OmsError::InvalidValue(_)
                | OmsError::InvalidName(_)
                | OmsError::NameNotFound { .. }
        ),
        VmError::Tf(_)
        | VmError::Value(_)
        | VmError::InvalidProcessState(_)
        | VmError::TokenPositionOutOfRange(_)
        | VmError::StepLimitExceeded(_) => false,
    }
}

fn error_value(error: &VmError) -> Value {
    let code = match error {
        VmError::Oms(_) => "object_error",
        VmError::StackUnderflow => "stack_underflow",
        VmError::UndefinedVariable(_) => "undefined_variable",
        VmError::TypeError(_) => "type_error",
        VmError::DivisionByZero => "division_by_zero",
        VmError::IndexOutOfBounds => "index_out_of_bounds",
        VmError::MissingKey(_) => "missing_key",
        VmError::MissingProvider(_) => "missing_provider",
        VmError::Provider(_) => "provider_error",
        VmError::Tf(_) => "tf_error",
        VmError::Value(_) => "value_error",
        VmError::InvalidProcessState(_) => "invalid_process_state",
        VmError::TokenPositionOutOfRange(_) => "token_position_out_of_range",
        VmError::StepLimitExceeded(_) => "step_limit_exceeded",
    };
    Value::Error {
        code: code.to_owned(),
        message: error.to_string(),
    }
}

fn arithmetic(token: &Token, left: Value, right: Value) -> Result<Value, VmError> {
    if matches!(left, Value::Float(_)) || matches!(right, Value::Float(_)) {
        let left = numeric_float(&left)?;
        let right = numeric_float(&right)?;
        if matches!(token, Token::Divide | Token::Modulo) && right == 0.0 {
            return Err(VmError::DivisionByZero);
        }
        let value = match token {
            Token::Add => left + right,
            Token::Subtract => left - right,
            Token::Multiply => left * right,
            Token::Divide => left / right,
            Token::Modulo => left % right,
            _ => return Err(VmError::TypeError("invalid arithmetic operands")),
        };
        return Ok(Value::Float(oms_types::FloatValue::new(value)));
    }
    match (token, left, right) {
        (Token::Add, Value::Integer(left), Value::Integer(right)) => left
            .checked_add(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Subtract, Value::Integer(left), Value::Integer(right)) => left
            .checked_sub(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Multiply, Value::Integer(left), Value::Integer(right)) => left
            .checked_mul(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Divide | Token::Modulo, Value::Integer(_), Value::Integer(0)) => {
            Err(VmError::DivisionByZero)
        }
        (Token::Divide, Value::Integer(left), Value::Integer(right)) => left
            .checked_div(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Modulo, Value::Integer(left), Value::Integer(right)) => left
            .checked_rem(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Add, Value::Text(mut left), Value::Text(right)) => {
            left.push_str(&right);
            Ok(Value::Text(left))
        }
        _ => Err(VmError::TypeError("invalid arithmetic operands")),
    }
}

fn execute_collection_token(token: &Token, stack: &mut Vec<Value>) -> Result<(), VmError> {
    match token {
        Token::MakeArray(count) => {
            let count =
                usize::try_from(*count).map_err(|_| VmError::TypeError("array is too large"))?;
            let start = stack
                .len()
                .checked_sub(count)
                .ok_or(VmError::StackUnderflow)?;
            let values = stack.split_off(start);
            stack.push(Value::Array(values));
        }
        Token::MakeMap(count) => {
            let count =
                usize::try_from(*count).map_err(|_| VmError::TypeError("map is too large"))?;
            let item_count = count
                .checked_mul(2)
                .ok_or(VmError::TypeError("map is too large"))?;
            if stack.len() < item_count {
                return Err(VmError::StackUnderflow);
            }
            let mut values = BTreeMap::new();
            for _ in 0..count {
                let value = stack.pop().ok_or(VmError::StackUnderflow)?;
                let Value::Text(key) = stack.pop().ok_or(VmError::StackUnderflow)? else {
                    return Err(VmError::TypeError("map key must be text"));
                };
                values.insert(key, value);
            }
            stack.push(Value::Map(values));
        }
        Token::IndexGet => {
            let index = stack.pop().ok_or(VmError::StackUnderflow)?;
            let collection = stack.pop().ok_or(VmError::StackUnderflow)?;
            stack.push(index_get(collection, index)?);
        }
        Token::IndexSet => {
            let value = stack.pop().ok_or(VmError::StackUnderflow)?;
            let index = stack.pop().ok_or(VmError::StackUnderflow)?;
            let collection = stack.pop().ok_or(VmError::StackUnderflow)?;
            stack.push(index_set(collection, index, value)?);
        }
        Token::IndexIncrement | Token::IndexDecrement => {
            let index = stack.pop().ok_or(VmError::StackUnderflow)?;
            let collection = stack.pop().ok_or(VmError::StackUnderflow)?;
            let current = index_get(collection.clone(), index.clone())?;
            let operator = if matches!(token, Token::IndexIncrement) {
                Token::Add
            } else {
                Token::Subtract
            };
            let value = arithmetic(&operator, current, Value::Integer(1))?;
            stack.push(index_set(collection, index, value)?);
        }
        Token::Length => {
            let value = stack.pop().ok_or(VmError::StackUnderflow)?;
            stack.push(Value::Integer(value_length(&value)?));
        }
        _ => return Err(VmError::TypeError("token is not a collection operation")),
    }
    Ok(())
}

fn index_get(collection: Value, index: Value) -> Result<Value, VmError> {
    match (collection, index) {
        (Value::Array(values), Value::Integer(index)) => values
            .get(index_position(index)?)
            .cloned()
            .ok_or(VmError::IndexOutOfBounds),
        (Value::Map(values) | Value::Record(values), Value::Text(key)) => {
            values.get(&key).cloned().ok_or(VmError::MissingKey(key))
        }
        (Value::Text(value), Value::Integer(index)) => value
            .chars()
            .nth(index_position(index)?)
            .map(|character| Value::Text(character.to_string()))
            .ok_or(VmError::IndexOutOfBounds),
        _ => Err(VmError::TypeError("value does not support this index")),
    }
}

fn index_set(mut collection: Value, index: Value, value: Value) -> Result<Value, VmError> {
    match (&mut collection, index) {
        (Value::Array(values), Value::Integer(index)) => {
            let target = values
                .get_mut(index_position(index)?)
                .ok_or(VmError::IndexOutOfBounds)?;
            *target = value;
        }
        (Value::Map(values) | Value::Record(values), Value::Text(key)) => {
            values.insert(key, value);
        }
        _ => {
            return Err(VmError::TypeError(
                "value does not support indexed assignment",
            ));
        }
    }
    Ok(collection)
}

fn index_position(index: i64) -> Result<usize, VmError> {
    usize::try_from(index).map_err(|_| VmError::IndexOutOfBounds)
}

#[allow(clippy::too_many_lines)]
fn math_capability(name: &str, arguments: &[Value]) -> Result<Value, VmError> {
    match (name, arguments) {
        ("random", []) => random_float(),
        ("random_integer", [Value::Integer(minimum), Value::Integer(maximum)]) => {
            if minimum > maximum {
                return Err(VmError::TypeError(
                    "math.random_integer minimum exceeds maximum",
                ));
            }
            Ok(Value::Integer(random_integer_inclusive(
                *minimum, *maximum,
            )?))
        }
        ("abs", [Value::Integer(value)]) => value
            .checked_abs()
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow in math.abs")),
        ("abs", [Value::Float(value)]) => finite_math_result(value.get().abs()),
        ("min", [left, right]) => Ok(if numeric_order(left, right)?.is_gt() {
            right.clone()
        } else {
            left.clone()
        }),
        ("max", [left, right]) => Ok(if numeric_order(left, right)?.is_lt() {
            right.clone()
        } else {
            left.clone()
        }),
        ("clamp", [value, minimum, maximum]) => {
            if numeric_order(minimum, maximum)?.is_gt() {
                return Err(VmError::TypeError("math.clamp minimum exceeds maximum"));
            }
            if numeric_order(value, minimum)?.is_lt() {
                Ok(minimum.clone())
            } else if numeric_order(value, maximum)?.is_gt() {
                Ok(maximum.clone())
            } else {
                Ok(value.clone())
            }
        }
        ("sqrt", [value]) => {
            let value = finite_math_number(value)?;
            if value < 0.0 {
                return Err(VmError::TypeError(
                    "math.sqrt requires a non-negative number",
                ));
            }
            finite_math_result(value.sqrt())
        }
        ("pow", [base, exponent]) => {
            let base = finite_math_number(base)?;
            let exponent = finite_math_number(exponent)?;
            finite_math_result(base.powf(exponent))
        }
        ("floor", [value]) => rounded_math_value(value, f64::floor),
        ("ceil", [value]) => rounded_math_value(value, f64::ceil),
        ("round", [value]) => rounded_math_value(value, f64::round),
        ("trunc", [value]) => rounded_math_value(value, f64::trunc),
        ("sin", [value]) => finite_math_result(finite_math_number(value)?.sin()),
        ("cos", [value]) => finite_math_result(finite_math_number(value)?.cos()),
        ("tan", [value]) => finite_math_result(finite_math_number(value)?.tan()),
        ("atan2", [y, x]) => {
            finite_math_result(finite_math_number(y)?.atan2(finite_math_number(x)?))
        }
        ("hypot", [x, y]) => {
            finite_math_result(finite_math_number(x)?.hypot(finite_math_number(y)?))
        }
        ("log", [value]) => {
            let value = finite_math_number(value)?;
            if value <= 0.0 {
                return Err(VmError::TypeError("math.log requires a positive number"));
            }
            finite_math_result(value.ln())
        }
        ("log2", [value]) => {
            let value = finite_math_number(value)?;
            if value <= 0.0 {
                return Err(VmError::TypeError("math.log2 requires a positive number"));
            }
            finite_math_result(value.log2())
        }
        ("log10", [value]) => {
            let value = finite_math_number(value)?;
            if value <= 0.0 {
                return Err(VmError::TypeError("math.log10 requires a positive number"));
            }
            finite_math_result(value.log10())
        }
        ("exp", [value]) => finite_math_result(finite_math_number(value)?.exp()),
        _ => Err(VmError::TypeError("invalid arguments to math capability")),
    }
}

#[allow(clippy::cast_precision_loss)]
fn random_float() -> Result<Value, VmError> {
    // 53 random bits are represented exactly by an IEEE-754 f64 significand.
    const DENOMINATOR: f64 = 9_007_199_254_740_992.0;
    let mantissa = random_u64()? >> 11;
    Ok(Value::Float(oms_types::FloatValue::new(
        mantissa as f64 / DENOMINATOR,
    )))
}

fn random_integer_inclusive(minimum: i64, maximum: i64) -> Result<i64, VmError> {
    const SIGN_BIT: u64 = 1_u64 << 63;
    let minimum_ordered = u64::from_ne_bytes(minimum.to_ne_bytes()) ^ SIGN_BIT;
    let maximum_ordered = u64::from_ne_bytes(maximum.to_ne_bytes()) ^ SIGN_BIT;
    let range = maximum_ordered
        .wrapping_sub(minimum_ordered)
        .wrapping_add(1);
    let offset = if range == 0 {
        random_u64()?
    } else {
        let rejection_threshold = range.wrapping_neg() % range;
        loop {
            let sample = random_u64()?;
            if sample >= rejection_threshold {
                break sample % range;
            }
        }
    };
    let ordered = minimum_ordered.wrapping_add(offset) ^ SIGN_BIT;
    Ok(i64::from_ne_bytes(ordered.to_ne_bytes()))
}

fn random_u64() -> Result<u64, VmError> {
    let mut bytes = [0_u8; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| VmError::Provider(format!("system random source failed: {error}")))?;
    Ok(u64::from_ne_bytes(bytes))
}

fn text_capability(name: &str, arguments: &[Value], receiver: &Value) -> Result<Value, VmError> {
    let Value::Text(text) = receiver else {
        return Err(VmError::TypeError("text capability requires a Text Object"));
    };
    match (name, arguments) {
        ("slice", [Value::Integer(start), Value::Integer(length)]) => {
            let characters: Vec<char> = text.chars().collect();
            let start = usize::try_from(*start)
                .map_err(|_| VmError::TypeError("text slice start must be non-negative"))?;
            let length = usize::try_from(*length)
                .map_err(|_| VmError::TypeError("text slice length must be non-negative"))?;
            let end = start
                .checked_add(length)
                .filter(|end| *end <= characters.len())
                .ok_or(VmError::IndexOutOfBounds)?;
            Ok(Value::Text(characters[start..end].iter().collect()))
        }
        ("find", [Value::Text(needle)]) => {
            let position = text.find(needle).map(|byte| text[..byte].chars().count());
            Ok(position.map_or(Value::Integer(-1), |position| {
                Value::Integer(i64::try_from(position).unwrap_or(i64::MAX))
            }))
        }
        ("contains", [Value::Text(needle)]) => Ok(Value::Bool(text.contains(needle))),
        ("split", [Value::Text(delimiter)]) => {
            let parts = if delimiter.is_empty() {
                text.chars()
                    .map(|character| Value::Text(character.to_string()))
                    .collect()
            } else {
                text.split(delimiter)
                    .map(|part| Value::Text(part.to_owned()))
                    .collect()
            };
            Ok(Value::Array(parts))
        }
        ("replace_all", [Value::Text(from), Value::Text(to)]) => {
            Ok(Value::Text(text.replace(from, to)))
        }
        ("trim", []) => Ok(Value::Text(text.trim().to_owned())),
        ("lower", []) => Ok(Value::Text(text.to_lowercase())),
        ("upper", []) => Ok(Value::Text(text.to_uppercase())),
        _ => Err(VmError::TypeError("invalid arguments to text capability")),
    }
}

fn finite_math_number(value: &Value) -> Result<f64, VmError> {
    let number = numeric_float(value)?;
    if !number.is_finite() {
        return Err(VmError::TypeError("math inputs must be finite numbers"));
    }
    Ok(number)
}

fn finite_math_result(value: f64) -> Result<Value, VmError> {
    if !value.is_finite() {
        return Err(VmError::TypeError(
            "math result is outside the finite range",
        ));
    }
    Ok(Value::Float(oms_types::FloatValue::new(value)))
}

fn rounded_math_value(value: &Value, round: fn(f64) -> f64) -> Result<Value, VmError> {
    match value {
        Value::Integer(_) => Ok(value.clone()),
        Value::Float(_) => finite_math_result(round(finite_math_number(value)?)),
        _ => Err(VmError::TypeError("math function requires a number")),
    }
}

fn numeric_order(left: &Value, right: &Value) -> Result<std::cmp::Ordering, VmError> {
    match (left, right) {
        (Value::Integer(left), Value::Integer(right)) => Ok(left.cmp(right)),
        (Value::Integer(integer), Value::Float(_)) => {
            Ok(compare_integer_float(*integer, finite_math_number(right)?))
        }
        (Value::Float(_), Value::Integer(integer)) => {
            Ok(compare_integer_float(*integer, finite_math_number(left)?).reverse())
        }
        (Value::Float(_), Value::Float(_)) => finite_math_number(left)?
            .partial_cmp(&finite_math_number(right)?)
            .ok_or(VmError::TypeError(
                "math comparison requires finite numbers",
            )),
        _ => Err(VmError::TypeError("math comparison requires numbers")),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn compare_integer_float(integer: i64, float: f64) -> std::cmp::Ordering {
    if float >= 9_223_372_036_854_775_808.0 {
        return std::cmp::Ordering::Less;
    }
    if float < -9_223_372_036_854_775_808.0 {
        return std::cmp::Ordering::Greater;
    }
    let truncated = float.trunc() as i64;
    match integer.cmp(&truncated) {
        std::cmp::Ordering::Equal if float.fract() > 0.0 => std::cmp::Ordering::Less,
        std::cmp::Ordering::Equal if float.fract() < 0.0 => std::cmp::Ordering::Greater,
        ordering => ordering,
    }
}

fn value_length(value: &Value) -> Result<i64, VmError> {
    let length = match value {
        Value::Text(value) => value.chars().count(),
        Value::Bytes(value) => value.len(),
        Value::Array(value) => value.len(),
        Value::Map(value) | Value::Record(value) => value.len(),
        _ => return Err(VmError::TypeError("value has no length")),
    };
    i64::try_from(length).map_err(|_| VmError::TypeError("length exceeds integer range"))
}

fn compare(token: &Token, left: &Value, right: &Value) -> Result<bool, VmError> {
    match token {
        Token::Equal => Ok(left == right),
        Token::NotEqual => Ok(left != right),
        Token::Less | Token::LessEqual | Token::Greater | Token::GreaterEqual => {
            let left = numeric_float(left)?;
            let right = numeric_float(right)?;
            if left.is_nan() || right.is_nan() {
                return Err(VmError::TypeError("NaN cannot be ordered"));
            }
            Ok(match token {
                Token::Less => left < right,
                Token::LessEqual => left <= right,
                Token::Greater => left > right,
                Token::GreaterEqual => left >= right,
                _ => unreachable!(),
            })
        }
        _ => Err(VmError::TypeError("token is not a comparison")),
    }
}

#[allow(clippy::cast_precision_loss)]
fn numeric_float(value: &Value) -> Result<f64, VmError> {
    match value {
        Value::Integer(value) => Ok(*value as f64),
        Value::Float(value) => Ok(value.get()),
        _ => Err(VmError::TypeError(
            "numeric operation requires integers or floats",
        )),
    }
}

fn encode_process_state(state: &ProcessState) -> Result<Vec<u8>, VmError> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(PROCESS_MAGIC);
    write_u128(&mut bytes, state.program.as_u128());
    write_u128(&mut bytes, state.subject.as_u128());
    write_u32(&mut bytes, state.token_position);
    bytes.push(match state.status {
        ProcessStatus::Running => 0,
        ProcessStatus::Suspended => 1,
        ProcessStatus::Halted => 2,
        ProcessStatus::Terminated => 3,
        ProcessStatus::Failed => 4,
    });
    write_u32(&mut bytes, state_len(state.stack.len())?);
    for value in &state.stack {
        encode_value(&mut bytes, value)?;
    }
    write_u32(&mut bytes, state_len(state.variables.len())?);
    for (name, object) in &state.variables {
        write_string(&mut bytes, name)?;
        write_u128(&mut bytes, object.as_u128());
    }
    write_u32(&mut bytes, state_len(state.frames.len())?);
    for frame in &state.frames {
        write_u32(&mut bytes, frame.return_position);
        write_u32(&mut bytes, frame.stack_base);
        write_u32(&mut bytes, state_len(frame.locals.len())?);
        for (name, object) in &frame.locals {
            write_string(&mut bytes, name)?;
            write_u128(&mut bytes, object.as_u128());
        }
        match frame.receiver {
            Some(receiver) => {
                bytes.push(1);
                write_u128(&mut bytes, receiver.as_u128());
            }
            None => bytes.push(0),
        }
        match &frame.class {
            Some(class) => {
                bytes.push(1);
                write_string(&mut bytes, class)?;
            }
            None => bytes.push(0),
        }
    }
    write_u32(&mut bytes, state_len(state.handlers.len())?);
    for handler in &state.handlers {
        write_u32(&mut bytes, handler.catch_position);
        write_string(&mut bytes, &handler.error_name)?;
        write_u32(&mut bytes, handler.frame_depth);
        write_u32(&mut bytes, handler.stack_base);
    }
    bytes.extend_from_slice(PROCESS_RESULT_EXTENSION);
    encode_optional_value(&mut bytes, state.result.as_ref())?;
    encode_optional_value(&mut bytes, state.error.as_ref())?;
    bytes.extend_from_slice(PROCESS_RUNTIME_EXTENSION);
    encode_optional_u64(&mut bytes, state.wake_at_unix_ms);
    encode_optional_u64(&mut bytes, state.ended_at_unix_ms);
    Ok(bytes)
}

fn encode_optional_value(bytes: &mut Vec<u8>, value: Option<&Value>) -> Result<(), VmError> {
    match value {
        Some(value) => {
            bytes.push(1);
            encode_value(bytes, value)?;
        }
        None => bytes.push(0),
    }
    Ok(())
}

fn decode_optional_value(reader: &mut StateReader<'_>) -> Result<Option<Value>, VmError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(decode_value(reader)?)),
        _ => Err(invalid_state("invalid Process optional-value marker")),
    }
}

fn encode_optional_u64(bytes: &mut Vec<u8>, value: Option<u64>) {
    match value {
        Some(value) => {
            bytes.push(1);
            write_u64(bytes, value);
        }
        None => bytes.push(0),
    }
}

fn reap_expired_processes(
    manager: &Arc<InMemoryObjectManager>,
    now: SystemTime,
) -> Result<(), VmError> {
    let now = now
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX);
    let cutoff = now.saturating_sub(PROCESS_RETENTION_MILLIS);
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let processes = manager.query(context, &ObjectQuery::new().with_type(PROCESS_TYPE))?;
    let mut retired = BTreeSet::new();
    let mut discovery_order = Vec::new();
    for header in processes {
        if retired.contains(&header.id) {
            continue;
        }
        let view = match manager.read(context, header.id) {
            Ok(view) => view,
            Err(OmsError::InvalidLifecycle { .. }) => continue,
            Err(error) => return Err(error.into()),
        };
        let state = decode_process_state(view.state())?;
        if !matches!(
            state.status,
            ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
        ) || state.ended_at_unix_ms.is_none_or(|ended| ended > cutoff)
        {
            continue;
        }
        if let Some(tree) = collect_expired_process_tree(manager, header.id, cutoff)? {
            for object in tree {
                if retired.insert(object) {
                    discovery_order.push(object);
                }
            }
        }
    }
    if retired.is_empty() {
        Ok(())
    } else {
        retire_process_objects(manager, &retired, &discovery_order)
    }
}

fn process_cleanup_deadline(
    manager: &InMemoryObjectManager,
    process: ObjectId,
) -> Result<Option<u64>, VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let header = manager.inspect(context, process)?;
    if header.type_id != PROCESS_TYPE || header.lifecycle == LifecycleState::Tombstoned {
        return Ok(None);
    }
    let state = decode_process_state(manager.read(context, process)?.state())?;
    if !matches!(
        state.status,
        ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
    ) {
        return Ok(None);
    }
    Ok(state
        .ended_at_unix_ms
        .map(|ended| ended.saturating_add(PROCESS_RETENTION_MILLIS)))
}

fn finished_process_deadlines(
    manager: &Arc<InMemoryObjectManager>,
) -> Result<BTreeSet<(u64, ObjectId)>, VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    manager
        .query(context, &ObjectQuery::new().with_type(PROCESS_TYPE))?
        .into_iter()
        .filter_map(
            |header| match process_cleanup_deadline(manager, header.id) {
                Ok(Some(deadline)) => Some(Ok((deadline, header.id))),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            },
        )
        .collect()
}

fn process_ancestors(
    manager: &InMemoryObjectManager,
    process: ObjectId,
) -> Result<Vec<ObjectId>, VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let mut ancestors = Vec::new();
    let mut parent = manager.inspect(context, process)?.parent_id;
    while let Some(object) = parent {
        let header = manager.inspect(context, object)?;
        if header.type_id == PROCESS_TYPE {
            ancestors.push(object);
        }
        parent = header.parent_id;
    }
    Ok(ancestors)
}

fn schedule_finished_process(
    manager: &InMemoryObjectManager,
    process: ObjectId,
    indexed: &mut BTreeMap<ObjectId, u64>,
    deadlines: &mut BTreeSet<(u64, ObjectId)>,
) -> Result<(), VmError> {
    if let Some(previous) = indexed.remove(&process) {
        deadlines.remove(&(previous, process));
    }
    if let Some(deadline) = process_cleanup_deadline(manager, process)? {
        indexed.insert(process, deadline);
        deadlines.insert((deadline, process));
    }
    Ok(())
}

fn process_reaper_loop(
    manager: &Arc<InMemoryObjectManager>,
    initial_deadlines: BTreeSet<(u64, ObjectId)>,
    receiver: &mpsc::Receiver<ProcessReaperMessage>,
    interval: Duration,
) {
    let mut deadlines = initial_deadlines;
    let mut indexed = deadlines
        .iter()
        .map(|(deadline, process)| (*process, *deadline))
        .collect::<BTreeMap<_, _>>();
    let interval = interval.max(Duration::from_secs(1));
    loop {
        let now = unix_time_millis();
        let due = deadlines
            .iter()
            .take_while(|(deadline, _)| *deadline <= now)
            .copied()
            .collect::<Vec<_>>();
        if !due.is_empty() {
            let mut ancestors = BTreeSet::new();
            for (_, process) in &due {
                if let Ok(found) = process_ancestors(manager, *process) {
                    ancestors.extend(found);
                }
            }
            match reap_expired_processes(manager, SystemTime::now()) {
                Ok(()) => {
                    for (deadline, process) in due {
                        deadlines.remove(&(deadline, process));
                        indexed.remove(&process);
                    }
                }
                Err(error) => {
                    eprintln!("Ousject Process cleanup failed: {error}");
                    let retry_at =
                        now.saturating_add(interval.as_millis().try_into().unwrap_or(u64::MAX));
                    for (_, process) in due {
                        if let Some(previous) = indexed.insert(process, retry_at) {
                            deadlines.remove(&(previous, process));
                        }
                        deadlines.insert((retry_at, process));
                    }
                }
            }
            for ancestor in ancestors {
                if let Err(error) =
                    schedule_finished_process(manager, ancestor, &mut indexed, &mut deadlines)
                {
                    eprintln!("Ousject Process cleanup scheduling failed: {error}");
                }
            }
            continue;
        }

        let delay = deadlines
            .first()
            .map(|(deadline, _)| Duration::from_millis(deadline.saturating_sub(now)))
            .map_or(interval, |delay| delay.min(interval));
        match receiver.recv_timeout(delay) {
            Ok(ProcessReaperMessage::Shutdown) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                break;
            }
            Ok(ProcessReaperMessage::Wake(process)) => {
                let mut wake = BTreeSet::from([process]);
                if let Ok(ancestors) = process_ancestors(manager, process) {
                    wake.extend(ancestors);
                }
                for process in wake {
                    if let Err(error) =
                        schedule_finished_process(manager, process, &mut indexed, &mut deadlines)
                    {
                        eprintln!("Ousject Process cleanup scheduling failed: {error}");
                    }
                }
            }
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
    }
}

fn collect_expired_process_tree(
    manager: &Arc<InMemoryObjectManager>,
    root: ObjectId,
    cutoff: u64,
) -> Result<Option<Vec<ObjectId>>, VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let mut retired = BTreeSet::new();
    let mut discovery_order = Vec::new();
    let mut pending = vec![root];
    while let Some(object) = pending.pop() {
        if !retired.insert(object) {
            continue;
        }
        let view = manager.read(context, object)?;
        if view.header().type_id == CORE_PROCESS_TYPE {
            let state = decode_process_state(view.state())?;
            if !matches!(
                state.status,
                ProcessStatus::Halted | ProcessStatus::Terminated | ProcessStatus::Failed
            ) || state.ended_at_unix_ms.is_none_or(|ended| ended > cutoff)
                || view.links().contains_key("$effect")
            {
                return Ok(None);
            }
        }
        if view.header().type_id == CORE_EFFECT_TYPE {
            let effect = EffectRecord::decode(view.state())?;
            if effect.status == ousject_provider::EffectStatus::Pending {
                return Ok(None);
            }
        }
        discovery_order.push(object);
        pending.extend(view.children().iter().copied());
    }

    Ok(Some(discovery_order))
}

fn retire_process_objects(
    manager: &Arc<InMemoryObjectManager>,
    retired: &BTreeSet<ObjectId>,
    discovery_order: &[ObjectId],
) -> Result<(), VmError> {
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let mut transaction = manager.begin(context);
    for header in manager.list(context)? {
        if header.lifecycle == LifecycleState::Tombstoned {
            continue;
        }
        let view = manager.read(context, header.id)?;
        let mut remove_links = view
            .links()
            .iter()
            .filter(|(_, target)| retired.contains(target))
            .map(|(name, _)| name.clone())
            .collect::<BTreeSet<_>>();
        if retired.contains(&header.id) {
            remove_links.extend(view.links().keys().cloned());
        }
        if !remove_links.is_empty() {
            transaction.expect(header.id, header.version);
            for name in remove_links {
                transaction.remove_link(header.id, name);
            }
        }
        if header.type_id == PROCESS_TYPE && !retired.contains(&header.id) {
            let mut process = decode_process_state(view.state())?;
            if remove_retired_bindings(&mut process, retired) {
                transaction
                    .expect(header.id, header.version)
                    .update_state(header.id, encode_process_state(&process)?);
            }
        }
    }
    for object in discovery_order.iter().rev().copied() {
        let view = manager.read(context, object)?;
        transaction.expect(object, view.header().version);
        if let Some(parent) = view.header().parent_id {
            let parent_version = manager.inspect(context, parent)?.version;
            transaction.expect(parent, parent_version);
        }
        transaction.tombstone(object);
    }
    manager.commit(transaction)?;
    Ok(())
}

fn decode_optional_u64(reader: &mut StateReader<'_>) -> Result<Option<u64>, VmError> {
    match reader.u8()? {
        0 => Ok(None),
        1 => Ok(Some(reader.u64()?)),
        _ => Err(invalid_state("invalid Process optional-time marker")),
    }
}

#[allow(clippy::too_many_lines)]
fn object_essential_value(view: &oms_runtime::ObjectView) -> Result<Value, VmError> {
    if view.header().type_id != PROCESS_TYPE {
        return Ok(
            Value::decode(view.state()).unwrap_or_else(|_| Value::Bytes(view.state().to_vec()))
        );
    }

    let process = decode_process_state(view.state())?;
    let variables = process
        .variables
        .iter()
        .map(|(name, object)| (name.clone(), Value::Text(object.to_string())))
        .collect();
    let frames = process
        .frames
        .iter()
        .map(|frame| {
            Value::Record(BTreeMap::from([
                (
                    "return_position".to_owned(),
                    Value::Integer(i64::from(frame.return_position)),
                ),
                (
                    "stack_base".to_owned(),
                    Value::Integer(i64::from(frame.stack_base)),
                ),
                (
                    "locals".to_owned(),
                    Value::Record(
                        frame
                            .locals
                            .iter()
                            .map(|(name, object)| (name.clone(), Value::Text(object.to_string())))
                            .collect(),
                    ),
                ),
                (
                    "receiver".to_owned(),
                    frame
                        .receiver
                        .map_or(Value::Null, |object| Value::Text(object.to_string())),
                ),
                (
                    "class".to_owned(),
                    frame
                        .class
                        .as_ref()
                        .map_or(Value::Null, |class| Value::Text(class.clone())),
                ),
            ]))
        })
        .collect();
    let handlers = process
        .handlers
        .iter()
        .map(|handler| {
            Value::Record(BTreeMap::from([
                (
                    "catch_position".to_owned(),
                    Value::Integer(i64::from(handler.catch_position)),
                ),
                ("error".to_owned(), Value::Text(handler.error_name.clone())),
                (
                    "frame_depth".to_owned(),
                    Value::Integer(i64::from(handler.frame_depth)),
                ),
                (
                    "stack_base".to_owned(),
                    Value::Integer(i64::from(handler.stack_base)),
                ),
            ]))
        })
        .collect();
    Ok(Value::Record(BTreeMap::from([
        (
            "program".to_owned(),
            Value::Text(process.program.to_string()),
        ),
        (
            "subject".to_owned(),
            Value::Text(process.subject.to_string()),
        ),
        (
            "position".to_owned(),
            Value::Integer(i64::from(process.token_position)),
        ),
        (
            "status".to_owned(),
            Value::Text(process_status_name(process.status).to_owned()),
        ),
        (
            "wake_at_unix_ms".to_owned(),
            process.wake_at_unix_ms.map_or(Value::Null, |value| {
                Value::Integer(i64::try_from(value).unwrap_or(i64::MAX))
            }),
        ),
        (
            "ended_at_unix_ms".to_owned(),
            process.ended_at_unix_ms.map_or(Value::Null, |value| {
                Value::Integer(i64::try_from(value).unwrap_or(i64::MAX))
            }),
        ),
        ("stack".to_owned(), Value::Array(process.stack)),
        ("variables".to_owned(), Value::Record(variables)),
        ("frames".to_owned(), Value::Array(frames)),
        ("handlers".to_owned(), Value::Array(handlers)),
        (
            "result".to_owned(),
            process.result.clone().unwrap_or(Value::Null),
        ),
        (
            "error".to_owned(),
            process.error.clone().unwrap_or(Value::Null),
        ),
    ])))
}

#[allow(clippy::too_many_lines)]
fn decode_process_state(bytes: &[u8]) -> Result<ProcessState, VmError> {
    let mut reader = StateReader::new(bytes);
    if reader.take(4)? != PROCESS_MAGIC {
        return Err(invalid_state("invalid Process state magic"));
    }
    let program = ObjectId::from_u128(reader.u128()?);
    let subject = SubjectId::from_u128(reader.u128()?);
    let token_position = reader.u32()?;
    let status = match reader.u8()? {
        0 => ProcessStatus::Running,
        1 => ProcessStatus::Suspended,
        2 => ProcessStatus::Halted,
        3 => ProcessStatus::Terminated,
        4 => ProcessStatus::Failed,
        _ => return Err(invalid_state("invalid Process status")),
    };
    let mut stack = Vec::new();
    for _ in 0..reader.count()? {
        stack.push(decode_value(&mut reader)?);
    }
    let mut variables = BTreeMap::new();
    for _ in 0..reader.count()? {
        let name = reader.string()?;
        let object = ObjectId::from_u128(reader.u128()?);
        if variables.insert(name, object).is_some() {
            return Err(invalid_state("duplicate variable name"));
        }
    }
    let mut frames = Vec::new();
    for _ in 0..reader.count()? {
        let return_position = reader.u32()?;
        let stack_base = reader.u32()?;
        let mut locals = BTreeMap::new();
        for _ in 0..reader.count()? {
            let name = reader.string()?;
            let object = ObjectId::from_u128(reader.u128()?);
            if locals.insert(name, object).is_some() {
                return Err(invalid_state("duplicate local variable name"));
            }
        }
        let receiver = match reader.u8()? {
            0 => None,
            1 => Some(ObjectId::from_u128(reader.u128()?)),
            _ => return Err(invalid_state("invalid receiver marker")),
        };
        let class = match reader.u8()? {
            0 => None,
            1 => Some(reader.string()?),
            _ => return Err(invalid_state("invalid class marker")),
        };
        frames.push(CallFrame {
            return_position,
            stack_base,
            locals,
            receiver,
            class,
        });
    }
    let mut handlers = Vec::new();
    for _ in 0..reader.count()? {
        handlers.push(ExceptionHandler {
            catch_position: reader.u32()?,
            error_name: reader.string()?,
            frame_depth: reader.u32()?,
            stack_base: reader.u32()?,
        });
    }
    let (result, error, wake_at_unix_ms, ended_at_unix_ms) = if reader.is_empty() {
        (None, None, None, None)
    } else {
        if reader.take(4)? != PROCESS_RESULT_EXTENSION {
            return Err(invalid_state("unknown Process state extension"));
        }
        let result = decode_optional_value(&mut reader)?;
        let error = decode_optional_value(&mut reader)?;
        let (wake_at_unix_ms, ended_at_unix_ms) = if reader.is_empty() {
            (None, None)
        } else {
            if reader.take(4)? != PROCESS_RUNTIME_EXTENSION {
                return Err(invalid_state("unknown Process runtime extension"));
            }
            (
                decode_optional_u64(&mut reader)?,
                decode_optional_u64(&mut reader)?,
            )
        };
        (result, error, wake_at_unix_ms, ended_at_unix_ms)
    };
    if !reader.is_empty() {
        return Err(invalid_state("trailing Process state data"));
    }
    Ok(ProcessState {
        program,
        subject,
        token_position,
        stack,
        variables,
        status,
        result,
        error,
        wake_at_unix_ms,
        ended_at_unix_ms,
        frames,
        handlers,
    })
}

fn decode_value_state(bytes: &[u8]) -> Result<Value, VmError> {
    Value::decode(bytes).map_err(VmError::from)
}

fn encode_value(bytes: &mut Vec<u8>, value: &Value) -> Result<(), VmError> {
    let encoded = value.encode()?;
    write_u32(bytes, state_len(encoded.len())?);
    bytes.extend_from_slice(&encoded);
    Ok(())
}

fn decode_value(reader: &mut StateReader<'_>) -> Result<Value, VmError> {
    Value::decode(reader.bytes()?).map_err(VmError::from)
}

fn write_string(bytes: &mut Vec<u8>, value: &str) -> Result<(), VmError> {
    write_u32(bytes, state_len(value.len())?);
    bytes.extend_from_slice(value.as_bytes());
    Ok(())
}

fn write_u32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn write_u128(bytes: &mut Vec<u8>, value: u128) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

fn state_len(value: usize) -> Result<u32, VmError> {
    if value > MAX_STATE_ITEMS {
        return Err(invalid_state("VM state item limit exceeded"));
    }
    u32::try_from(value).map_err(|_| invalid_state("VM state is too large"))
}

fn invalid_state(message: &str) -> VmError {
    VmError::InvalidProcessState(message.to_owned())
}

struct StateReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> StateReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], VmError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or_else(|| invalid_state("VM state position overflow"))?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or_else(|| invalid_state("truncated VM state"))?;
        self.position = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, VmError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, VmError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, VmError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn u128(&mut self) -> Result<u128, VmError> {
        let mut bytes = [0; 16];
        bytes.copy_from_slice(self.take(16)?);
        Ok(u128::from_le_bytes(bytes))
    }

    fn count(&mut self) -> Result<usize, VmError> {
        let value = usize::try_from(self.u32()?)
            .map_err(|_| invalid_state("VM state count is unsupported"))?;
        if value > MAX_STATE_ITEMS {
            return Err(invalid_state("VM state item limit exceeded"));
        }
        Ok(value)
    }

    fn string(&mut self) -> Result<String, VmError> {
        let length = self.count()?;
        let value = std::str::from_utf8(self.take(length)?)
            .map_err(|_| invalid_state("VM state string is not UTF-8"))?;
        Ok(value.to_owned())
    }

    fn bytes(&mut self) -> Result<&'a [u8], VmError> {
        let length = self.count()?;
        self.take(length)
    }

    fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug)]
    struct TestConsole;

    impl ConsoleProvider for TestConsole {
        fn println(&self, _text: &str) -> Result<(), String> {
            Ok(())
        }
    }

    fn vm_with_console(manager: Arc<InMemoryObjectManager>) -> VirtualMachine {
        let console =
            VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
        VirtualMachine::with_console(manager, console, Arc::new(TestConsole)).unwrap()
    }

    #[test]
    fn process_state_round_trip() {
        let state = ProcessState {
            program: ObjectId::new(),
            subject: SubjectId::new(),
            token_position: 42,
            stack: vec![Value::Integer(1), Value::Text("two".to_owned())],
            variables: BTreeMap::from([("answer".to_owned(), ObjectId::new())]),
            frames: Vec::new(),
            handlers: Vec::new(),
            status: ProcessStatus::Running,
            result: None,
            error: None,
            wake_at_unix_ms: None,
            ended_at_unix_ms: None,
        };
        assert_eq!(
            decode_process_state(&encode_process_state(&state).unwrap()),
            Ok(state)
        );
    }

    #[test]
    fn timer_sleep_suspends_without_blocking_the_process_worker() {
        let vm = vm_with_console(Arc::new(InMemoryObjectManager::new(1).unwrap()));
        let program = praxis_compiler::compile(
            "time = object.find(\"time\")\ntime.sleep(10000)\nanswer = 42",
        )
        .unwrap();
        let process = vm.create_process(&program).unwrap();
        let report = vm.run(process, 1_000).unwrap();
        assert_eq!(report.status, ProcessStatus::Suspended);
        let state = vm.process_state(process).unwrap();
        assert!(state.wake_at_unix_ms.is_some());
        assert_eq!(state.status, ProcessStatus::Suspended);
        assert!(!vm.wake_due_timer(process).unwrap());
    }

    #[test]
    fn scheduler_runs_other_processes_before_waking_a_timer_sleeper() {
        let vm = vm_with_console(Arc::new(InMemoryObjectManager::new(1).unwrap()));
        let sleeper = vm
            .create_process(
                &praxis_compiler::compile(
                    "time = object.find(\"time\")\ntime.sleep(20)\nfinished = true",
                )
                .unwrap(),
            )
            .unwrap();
        let worker = vm
            .create_process(&praxis_compiler::compile("finished = true").unwrap())
            .unwrap();
        let mut scheduler = CooperativeScheduler::new(&vm);
        scheduler.enqueue(sleeper);
        scheduler.enqueue(worker);
        let report = scheduler.run(1000).unwrap();
        assert_eq!(report.processes.len(), 2);
        assert!(
            report
                .processes
                .iter()
                .all(|process| process.status == ProcessStatus::Halted)
        );
    }

    #[test]
    fn expired_finished_process_tree_is_retired_but_program_is_not() {
        let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
        let vm = vm_with_console(Arc::clone(&manager));
        let program = praxis_compiler::compile("answer = 42").unwrap();
        let process = vm.create_process(&program).unwrap();
        assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
        let mut state = vm.process_state(process).unwrap();
        state.ended_at_unix_ms = Some(0);
        let view = manager
            .read(AccessContext::new(SYSTEM_SUBJECT), process)
            .unwrap();
        let mut transaction = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state).unwrap());
        manager.commit(transaction).unwrap();

        reap_expired_processes(
            &manager,
            UNIX_EPOCH + Duration::from_millis(PROCESS_RETENTION_MILLIS + 10),
        )
        .unwrap();

        let context = AccessContext::new(SYSTEM_SUBJECT);
        assert_eq!(
            manager.inspect(context, process).unwrap().lifecycle,
            LifecycleState::Tombstoned
        );
        assert_eq!(
            manager
                .inspect(context, state.variables["answer"])
                .unwrap()
                .lifecycle,
            LifecycleState::Tombstoned
        );
        assert_eq!(
            manager.inspect(context, state.program).unwrap().lifecycle,
            LifecycleState::Active
        );
    }

    #[test]
    fn process_reaper_wakes_for_a_newly_ended_process() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = vm_with_console(Arc::clone(&manager));
        let process = vm
            .create_process(&praxis_compiler::compile("answer = 42").unwrap())
            .unwrap();
        assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
        let reaper = ProcessReaper::start(&vm, Duration::from_secs(1)).unwrap();

        let mut state = vm.process_state(process).unwrap();
        state.ended_at_unix_ms = Some(0);
        let view = manager
            .read(AccessContext::new(SYSTEM_SUBJECT), process)
            .unwrap();
        let mut transaction = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state).unwrap());
        manager.commit(transaction).unwrap();
        vm.notify_process_ended(process).unwrap();

        let context = AccessContext::new(SYSTEM_SUBJECT);
        for _ in 0..100 {
            if manager.inspect(context, process).unwrap().lifecycle == LifecycleState::Tombstoned {
                drop(reaper);
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(reaper);
        panic!("the background Process reaper did not retire the expired Process");
    }

    #[test]
    fn rejects_old_process_versions() {
        assert!(matches!(
            decode_process_state(b"OPS1"),
            Err(VmError::InvalidProcessState(_))
        ));
    }

    #[test]
    fn executes_program_with_variable_objects() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = vm_with_console(manager);
        let program = praxis_compiler::compile(
            "console = object.find(\"console\")\nanswer = 40 + 2\nconsole.println(answer)",
        )
        .unwrap();
        let process = vm.create_process(&program).unwrap();
        let report = vm.run(process, 100).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted);
        assert_eq!(report.output, vec!["42"]);
        assert_eq!(vm.variable(process, "answer"), Ok(Value::Integer(42)));
        assert_eq!(vm.process_state(process).unwrap().variables.len(), 2);
        vm.manager().health_check().unwrap();
    }

    #[test]
    fn console_is_shared_and_requires_a_provider_for_this_boot() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = vm_with_console(manager.clone());
        let program = praxis_compiler::compile(
            "console = object.find(\"console\")\nconsole.println(\"ready\")",
        )
        .unwrap();
        let first = vm.create_process(&program).unwrap();
        let second = vm.create_process(&program).unwrap();
        let context = AccessContext::new(SYSTEM_SUBJECT);
        let first_console = manager.read(context, first).unwrap().links()["console"];
        let second_console = manager.read(context, second).unwrap().links()["console"];
        assert_eq!(first_console, second_console);

        assert_eq!(vm.run(first, 2).unwrap().status, ProcessStatus::Running);
        let disconnected = VirtualMachine::new(manager);
        assert_eq!(
            disconnected.run(first, 10),
            Err(VmError::MissingProvider("console"))
        );
    }

    #[test]
    fn step_limit_preserves_resumable_position() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = VirtualMachine::new(manager);
        let program = Program {
            tokens: vec![Token::Jump(0), Token::Halt],
        };
        let process = vm.create_process(&program).unwrap();
        assert_eq!(vm.run(process, 5).unwrap().status, ProcessStatus::Running);
        assert_eq!(vm.process_state(process).unwrap().token_position, 0);
    }

    #[test]
    fn scheduler_runs_processes_round_robin() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = vm_with_console(manager);
        let first = vm
            .create_process(
                &praxis_compiler::compile(
                    "console = object.find(\"console\")\nconsole.println(\"a\")",
                )
                .unwrap(),
            )
            .unwrap();
        let second = vm
            .create_process(
                &praxis_compiler::compile(
                    "console = object.find(\"console\")\nconsole.println(\"b\")",
                )
                .unwrap(),
            )
            .unwrap();
        let mut scheduler = CooperativeScheduler::new(&vm);
        scheduler.enqueue(first);
        scheduler.enqueue(second);
        let report = scheduler.run(20).unwrap();
        assert_eq!(report.processes.len(), 2);
        assert!(
            report
                .processes
                .iter()
                .all(|process| process.status == ProcessStatus::Halted)
        );
        assert_eq!(report.total_steps, 14);
    }

    #[test]
    fn persistent_process_resumes_after_reopen() {
        let directory = std::env::temp_dir().join(format!("ousject-vm-{}", ObjectId::new()));
        let path = directory.join("objects.oms");
        let program = praxis_program_for_recovery();

        let process = {
            let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
            let vm = vm_with_console(manager);
            let process = vm.create_process(&program).unwrap();
            assert_eq!(vm.run(process, 5).unwrap().status, ProcessStatus::Running);
            process
        };

        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_console(manager);
        vm.reconnect_hardware(process).unwrap();
        let report = vm.run(process, 100).unwrap();
        assert_eq!(report.output, vec!["3"]);
        assert_eq!(vm.variable(process, "count"), Ok(Value::Integer(3)));
        drop(vm);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn praxis_program_for_recovery() -> Program {
        praxis_compiler::compile(
            "console = object.find(\"console\")\ncount = 0\nwhile count < 3 { count++ }\nconsole.println(count)",
        )
        .unwrap()
    }
}
