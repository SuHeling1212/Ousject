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
                oms_types::CORE_SWAP_POOL_TYPE,
                "core.swap_pool",
                ValueSchema::Record,
                CreationPolicy::Public,
                &["attach", "detach", "get", "list", "contains"],
            ),
            type_descriptor(
                oms_types::CORE_TIMER_TYPE,
                "core.timer",
                ValueSchema::Record,
                CreationPolicy::Public,
                &["arm", "wait", "cancel", "status"],
            ),
            type_descriptor(
                oms_types::CORE_AUDIT_TYPE,
                "core.audit",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                oms_types::CORE_AUDIT_EVENT_TYPE,
                "core.audit_event",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
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
                &["status", "result", "retry", "resolve"],
            ),
            type_descriptor(
                CORE_CONSOLE_TYPE,
                "core.console",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "print",
                    "println",
                    "render",
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
                oms_types::CORE_CRYPTO_TYPE,
                "core.crypto",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["sha256"],
            ),
            type_descriptor(
                CORE_TIME_TYPE,
                "core.time",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["now", "monotonic", "sleep"],
            ),
            type_descriptor(
                CORE_TERMINAL_SESSION_TYPE,
                "core.terminal_session",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "submit",
                    "history",
                    "pending_input",
                    "save_input",
                    "process",
                    "update_size",
                    "cancel",
                    "close",
                ],
            ),
            type_descriptor(
                CORE_TERMINAL_TYPE,
                "core.terminal",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["open"],
            ),
            type_descriptor(
                CORE_MODULE_TYPE,
                "core.module",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                CORE_MODULE_REGISTRY_TYPE,
                "core.module_registry",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "install",
                    "modules",
                    "find",
                    "enable",
                    "disable",
                    "instances",
                    "uninstall",
                    "upgrade",
                    "rollback",
                ],
            ),
            type_descriptor(
                CORE_MODULE_INSTANCE_TYPE,
                "core.module_instance",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_TYPE,
                "core.package",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_REGISTRY_TYPE,
                "core.package_registry",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "build",
                    "import",
                    "export",
                    "search",
                    "find",
                    "info",
                    "verify",
                    "install",
                    "list",
                    "require",
                    "restore",
                    "recover",
                ],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_INSTALLATION_TYPE,
                "core.package_installation",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "info",
                    "verify",
                    "module",
                    "resource",
                    "data",
                    "data_info",
                    "data_quota",
                    "set_data_quota",
                    "reset_data_quota",
                    "data_export",
                    "data_import",
                    "data_clear",
                    "run",
                    "upgrade",
                    "rollback",
                    "uninstall",
                ],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_SUBJECT_TYPE,
                "core.package_subject",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_INSTANCE_TYPE,
                "core.package_instance",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["process", "status"],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_AUDIT_TYPE,
                "core.package_audit",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_MARKET_TYPE,
                "core.package_market",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[
                    "configure",
                    "origin",
                    "update",
                    "search",
                    "info",
                    "download",
                    "install",
                    "list",
                ],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_MARKET_CONFIG_TYPE,
                "core.package_market_config",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_DOWNLOAD_TYPE,
                "core.package_download",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &["bytes", "info"],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_DATA_TYPE,
                "core.package_data",
                ValueSchema::Any,
                CreationPolicy::Public,
                &[],
            ),
            type_descriptor(
                oms_types::CORE_PACKAGE_MODULE_TYPE,
                "core.package_module",
                ValueSchema::Record,
                CreationPolicy::ProviderOnly,
                &[],
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
                &[
                    "capture",
                    "release",
                    "next_event",
                    "poll_event",
                    "poll_events",
                ],
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
