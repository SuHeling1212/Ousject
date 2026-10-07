#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    #[must_use]
    pub fn new(manager: Arc<InMemoryObjectManager>) -> Self {
        Self {
            manager,
            context: AccessContext::new(SYSTEM_SUBJECT),
            console_provider: None,
            console_driver: None,
            kernel_services: BTreeMap::new(),
            providers: Arc::new(ProviderRegistry::new()),
            program_cache: Arc::new(Mutex::new(BTreeMap::new())),
            package_verification_cache: Arc::new(Mutex::new(BTreeMap::new())),
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
            driver: Arc::clone(&driver),
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
            console_driver: Some(driver),
            kernel_services,
            providers,
            program_cache: Arc::new(Mutex::new(BTreeMap::new())),
            package_verification_cache: Arc::new(Mutex::new(BTreeMap::new())),
            process_reaper: Arc::new(Mutex::new(None)),
        })
    }

    /// Registers a domain-capability Provider during trusted boot, before the
    /// first Process is executed. The Provider Registry is sealed on the first
    /// runnable Process slice; later registration attempts fail.
    ///
    /// # Errors
    ///
    /// Returns an error if another Provider owns the Type or user-space has
    /// already caused the Registry to be sealed.
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
    pub(super) fn publish_kernel_services(
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
            ("crypto", CORE_CRYPTO_TYPE),
            ("time", CORE_TIME_TYPE),
            ("terminal", CORE_TERMINAL_TYPE),
            ("modules", CORE_MODULE_REGISTRY_TYPE),
            ("packages", CORE_PACKAGE_REGISTRY_TYPE),
            ("market", CORE_PACKAGE_MARKET_TYPE),
            ("audit", oms_types::CORE_AUDIT_TYPE),
            ("resolver", NET_RESOLVER_TYPE),
        ];
        let mut published = BTreeMap::new();
        for (name, type_id) in services {
            let object = if let Some(header) = manager
                .query(context, &ObjectQuery::new().with_type(type_id))?
                .into_iter()
                .find(|header| name != "terminal" || header.parent_id.is_none())
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
                let mut request = CreateObject::new(type_id, state.encode()?);
                if name == "audit" {
                    request.capabilities = [
                        Capability::Inspect,
                        Capability::ViewValue,
                        Capability::CreateChild,
                        Capability::Link,
                    ]
                    .into_iter()
                    .collect();
                }
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
        self.enforce_process_limits(None, subject)?;
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
                status: ProcessStatus::Ready,
                wait_reason: WaitReason::None,
                lease_owner: None,
                lease_generation: 0,
                lease_deadline_unix_ms: None,
                result: None,
                error: None,
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

    pub(super) fn grant_kernel_service_access(&self, subject: SubjectId) -> Result<(), VmError> {
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

    pub(super) fn stage_kernel_service_access(
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
            if self
                .kernel_services
                .iter()
                .any(|(name, service)| name == "audit" && *service == object)
            {
                continue;
            }
            let header = self.manager.inspect(context, object)?;
            let update = transaction
                .expect(object, header.version)
                .grant(object, subject, Capability::Inspect)
                .grant(object, subject, Capability::Invoke);
            let service_name = self
                .kernel_services
                .iter()
                .find_map(|(name, service)| (*service == object).then_some(name.as_str()));
            if !matches!(service_name, Some("packages" | "market")) {
                update.grant(object, subject, Capability::ViewValue);
            }
        }
        Ok(())
    }

    pub(super) fn require_local(&self) -> Result<(), VmError> {
        if self.context.subject == SYSTEM_SUBJECT {
            Ok(())
        } else {
            Err(VmError::Provider(
                "this kernel capability requires the local identity".to_owned(),
            ))
        }
    }

    pub(super) fn notify_process_ended(&self, process: ObjectId) -> Result<(), VmError> {
        self.wake_process_waiters(process)?;
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

    pub(super) fn wake_process_waiters(&self, process: ObjectId) -> Result<(), VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        for attempt in 0..3 {
            let child = self.manager.read(system, process)?;
            let waiters = child
                .links()
                .iter()
                .filter(|(name, _)| name.starts_with("$wait:"))
                .map(|(name, waiter)| (name.clone(), *waiter))
                .collect::<Vec<_>>();
            if waiters.is_empty() {
                return Ok(());
            }

            let mut transaction = self.manager.begin(system);
            transaction.expect(process, child.header().version);
            for (link, waiter) in waiters {
                let waiter_view = match self.manager.read(system, waiter) {
                    Ok(view) => view,
                    Err(OmsError::InvalidLifecycle { .. }) => {
                        transaction.remove_link(process, link);
                        continue;
                    }
                    Err(error) => return Err(error.into()),
                };
                let mut state = decode_process_state(waiter_view.state())?;
                if state.status == ProcessStatus::Waiting
                    && state.wait_reason == WaitReason::Process(process)
                    && waiter_view.links().get("$waiting_on") == Some(&process)
                {
                    state.status = ProcessStatus::Ready;
                    state.wait_reason = WaitReason::None;
                    if state.lease_owner.is_some() {
                        state.lease_generation = state
                            .lease_generation
                            .checked_add(1)
                            .ok_or_else(|| invalid_state("Worker lease generation overflow"))?;
                    }
                    state.lease_owner = None;
                    state.lease_deadline_unix_ms = None;
                    transaction
                        .expect(waiter, waiter_view.header().version)
                        .remove_link(waiter, "$waiting_on")
                        .update_state(waiter, encode_process_state(&state)?);
                }
                transaction.remove_link(process, link);
            }
            match self.manager.commit(transaction) {
                Ok(_) => return Ok(()),
                Err(OmsError::Conflict { .. }) if attempt < 2 => {}
                Err(error) => return Err(error.into()),
            }
        }
        Ok(())
    }

    pub(super) fn resolve_secret_text(&self, value: &str) -> Result<String, VmError> {
        if !value.starts_with("secret:") {
            return Ok(value.to_owned());
        }
        let provider = self.providers.get(CONSOLE_TYPE)?;
        provider.resolve_secret(value)?.ok_or_else(|| {
            VmError::Provider("secret input expired or belongs to another boot".to_owned())
        })
    }
}
