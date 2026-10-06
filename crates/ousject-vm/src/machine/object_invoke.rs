#![allow(clippy::wildcard_imports)]

use super::*;

impl VirtualMachine {
    #[allow(clippy::too_many_lines)]
    pub(super) fn invoke_object(
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
            (CORE_TERMINAL_TYPE, "open", []) => Ok((
                Value::Text(
                    self.open_terminal_session(current_state.subject)?
                        .to_string(),
                ),
                None,
            )),
            (CORE_TERMINAL_SESSION_TYPE, "submit", [Value::Text(source)]) => Ok((
                Value::Text(
                    self.terminal_session_submit(
                        object,
                        current_state.subject,
                        source,
                        transaction,
                    )?
                    .to_string(),
                ),
                None,
            )),
            (CORE_TERMINAL_SESSION_TYPE, "process", []) => Ok((
                Value::Text(
                    self.terminal_session_process(object, current_state.subject)?
                        .to_string(),
                ),
                None,
            )),
            (CORE_TERMINAL_SESSION_TYPE, "history", []) => Ok((
                self.terminal_session_history(object, current_state.subject)?,
                None,
            )),
            (CORE_TERMINAL_SESSION_TYPE, "pending_input", []) => Ok((
                self.terminal_session_pending_input(object, current_state.subject)?,
                None,
            )),
            (CORE_TERMINAL_SESSION_TYPE, "save_input", [Value::Text(source)]) => {
                self.terminal_session_save_input(
                    object,
                    current_state.subject,
                    source,
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (
                CORE_TERMINAL_SESSION_TYPE,
                "update_size",
                [Value::Integer(columns), Value::Integer(rows)],
            ) => {
                self.terminal_session_update_size(
                    object,
                    current_state.subject,
                    *columns,
                    *rows,
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (CORE_TERMINAL_SESSION_TYPE, "cancel", []) => {
                self.terminal_session_cancel(object, current_state.subject, transaction)?;
                Ok((Value::Null, None))
            }
            (CORE_TERMINAL_SESSION_TYPE, "close", []) => {
                let owner = current_state.subject;
                self.terminal_session_close(
                    current_process,
                    current_state,
                    object,
                    owner,
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (
                CORE_MODULE_REGISTRY_TYPE,
                "install",
                [
                    Value::Text(name),
                    Value::Text(version),
                    Value::Text(source),
                    Value::Array(capabilities),
                ],
            ) => Ok((
                Value::Text(
                    self.install_praxis_module(
                        current_state.subject,
                        name,
                        version,
                        source,
                        capabilities,
                        &[],
                    )?
                    .to_string(),
                ),
                None,
            )),
            (
                CORE_MODULE_REGISTRY_TYPE,
                "install",
                [
                    Value::Text(name),
                    Value::Text(version),
                    Value::Text(source),
                    Value::Array(capabilities),
                    Value::Array(dependencies),
                ],
            ) => Ok((
                Value::Text(
                    self.install_praxis_module(
                        current_state.subject,
                        name,
                        version,
                        source,
                        capabilities,
                        dependencies,
                    )?
                    .to_string(),
                ),
                None,
            )),
            (CORE_MODULE_REGISTRY_TYPE, "modules", []) => Ok((self.installed_modules()?, None)),
            (CORE_MODULE_REGISTRY_TYPE, "find", [Value::Text(name), Value::Text(version)]) => Ok((
                Value::Text(
                    self.find_praxis_module(current_state.subject, name, version)?
                        .to_string(),
                ),
                None,
            )),
            (CORE_PACKAGE_REGISTRY_TYPE, "build", [specification]) => {
                let artifact = self.build_package_artifact(
                    current_state.subject,
                    specification,
                    object,
                    transaction,
                )?;
                self.stage_package_audit(
                    current_state.subject,
                    object,
                    "build",
                    artifact,
                    Value::Record(BTreeMap::new()),
                    transaction,
                )?;
                Ok((Value::Text(artifact.to_string()), None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "import", [Value::Bytes(bytes)]) => {
                let artifact = self.import_package_artifact(bytes, None)?;
                self.stage_package_audit(
                    current_state.subject,
                    object,
                    "import",
                    artifact,
                    Value::Record(BTreeMap::new()),
                    transaction,
                )?;
                Ok((Value::Text(artifact.to_string()), None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "export", [Value::Text(artifact)]) => {
                let artifact = artifact
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Package Object id"))?;
                Ok((self.export_package_artifact(artifact)?, None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "search", [Value::Text(query)]) => {
                Ok((self.search_packages(query)?, None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "find", [Value::Text(coordinate)]) => Ok((
                Value::Text(self.find_package_artifact(coordinate)?.to_string()),
                None,
            )),
            (CORE_PACKAGE_REGISTRY_TYPE, "info", [Value::Text(artifact)]) => {
                let artifact = artifact
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Package Object id"))?;
                Ok((self.package_artifact_info(artifact)?, None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "verify", [Value::Text(artifact)]) => {
                let artifact = artifact
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Package Object id"))?;
                Ok((Value::Bool(self.verify_package_artifact(artifact)?), None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "install", [Value::Text(artifact)]) => {
                let artifact = artifact
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Package Object id"))?;
                let installation =
                    self.install_package(current_state.subject, object, artifact, transaction)?;
                self.stage_package_audit(
                    current_state.subject,
                    object,
                    "install",
                    installation,
                    Value::Record(BTreeMap::from([(
                        "package".to_owned(),
                        Value::Text(artifact.to_string()),
                    )])),
                    transaction,
                )?;
                Ok((Value::Text(installation.to_string()), None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "list", []) => {
                Ok((self.installed_packages(current_state.subject)?, None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "require", [Value::Text(coordinate)]) => Ok((
                Value::Text(
                    self.require_package(current_state.subject, coordinate)?
                        .to_string(),
                ),
                None,
            )),
            (CORE_PACKAGE_REGISTRY_TYPE, "recover", []) => {
                if current_state.subject != SYSTEM_SUBJECT {
                    return Err(VmError::TypeError(
                        "only local can inspect Package recovery state",
                    ));
                }
                Ok((self.package_recovery_report(current_state.subject)?, None))
            }
            (CORE_PACKAGE_REGISTRY_TYPE, "restore", [Value::Text(retired_installation)]) => {
                let retired_installation = retired_installation
                    .parse::<ObjectId>()
                    .map_err(|_| VmError::TypeError("invalid retired Package Installation ID"))?;
                let restored = self.restore_package(
                    current_state.subject,
                    object,
                    retired_installation,
                    transaction,
                )?;
                self.stage_package_audit(
                    current_state.subject,
                    object,
                    "restore",
                    restored,
                    Value::Record(BTreeMap::from([(
                        "retired_installation".to_owned(),
                        Value::Text(retired_installation.to_string()),
                    )])),
                    transaction,
                )?;
                Ok((Value::Text(restored.to_string()), None))
            }
            (CORE_PACKAGE_MARKET_TYPE, method, arguments) => {
                let result = self.market_invoke(
                    current_state.subject,
                    object,
                    method,
                    arguments,
                    transaction,
                )?;
                if method == "install" {
                    let Value::Text(installation) = &result else {
                        return Err(invalid_state("Market install returned no Installation ID"));
                    };
                    let installation = installation
                        .parse::<ObjectId>()
                        .map_err(|_| invalid_state("Market Installation ID is malformed"))?;
                    self.stage_package_audit(
                        current_state.subject,
                        self.package_registry_object()?,
                        "install",
                        installation,
                        Value::Record(BTreeMap::from([(
                            "source".to_owned(),
                            Value::Text("market".to_owned()),
                        )])),
                        transaction,
                    )?;
                }
                Ok((result, None))
            }
            (CORE_PACKAGE_DOWNLOAD_TYPE, method, []) => Ok((
                self.package_download_invoke(current_state.subject, object, method)?,
                None,
            )),
            (CORE_PACKAGE_INSTALLATION_TYPE, "info", []) => Ok((
                self.package_installation_info(current_state.subject, object)?,
                None,
            )),
            (CORE_PACKAGE_INSTALLATION_TYPE, "verify", []) => Ok((
                Value::Bool(self.verify_package_installation(current_state.subject, object)?),
                None,
            )),
            (CORE_PACKAGE_INSTALLATION_TYPE, "module", [Value::Text(name)]) => Ok((
                Value::Text(
                    self.package_module(current_state.subject, object, name)?
                        .to_string(),
                ),
                None,
            )),
            (CORE_PACKAGE_INSTALLATION_TYPE, "resource", [Value::Text(name)]) => Ok((
                self.package_resource(current_state.subject, object, name)?,
                None,
            )),
            (CORE_PACKAGE_INSTALLATION_TYPE, "run", []) => {
                let process = self.run_package_application(
                    current_state.subject,
                    object,
                    Value::Array(Vec::new()),
                    &[],
                    transaction,
                )?;
                self.stage_package_audit(
                    current_state.subject,
                    self.package_registry_object()?,
                    "run",
                    process,
                    Value::Record(BTreeMap::from([(
                        "installation".to_owned(),
                        Value::Text(object.to_string()),
                    )])),
                    transaction,
                )?;
                Ok((Value::Text(process.to_string()), None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "run", [arguments]) => {
                let process = self.run_package_application(
                    current_state.subject,
                    object,
                    arguments.clone(),
                    &[],
                    transaction,
                )?;
                self.stage_package_audit(
                    current_state.subject,
                    self.package_registry_object()?,
                    "run",
                    process,
                    Value::Record(BTreeMap::from([(
                        "installation".to_owned(),
                        Value::Text(object.to_string()),
                    )])),
                    transaction,
                )?;
                Ok((Value::Text(process.to_string()), None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "run", [arguments, Value::Array(capabilities)]) => {
                let capabilities = capabilities
                    .iter()
                    .map(|value| match value {
                        Value::Text(capability) => Ok(capability.clone()),
                        _ => Err(VmError::TypeError("Package capability grants must be Text")),
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                let process = self.run_package_application(
                    current_state.subject,
                    object,
                    arguments.clone(),
                    &capabilities,
                    transaction,
                )?;
                self.stage_package_audit(
                    current_state.subject,
                    self.package_registry_object()?,
                    "run",
                    process,
                    Value::Record(BTreeMap::from([
                        ("installation".to_owned(), Value::Text(object.to_string())),
                        (
                            "capabilities".to_owned(),
                            Value::Array(capabilities.into_iter().map(Value::Text).collect()),
                        ),
                    ])),
                    transaction,
                )?;
                Ok((Value::Text(process.to_string()), None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "upgrade", [Value::Text(artifact)]) => {
                let artifact = artifact
                    .parse::<ObjectId>()
                    .map_err(|_| VmError::TypeError("invalid Package Object ID"))?;
                let upgraded =
                    self.upgrade_package(current_state.subject, object, artifact, transaction)?;
                self.stage_package_audit(
                    current_state.subject,
                    self.package_registry_object()?,
                    "upgrade",
                    upgraded,
                    Value::Record(BTreeMap::from([
                        ("from".to_owned(), Value::Text(object.to_string())),
                        ("package".to_owned(), Value::Text(artifact.to_string())),
                    ])),
                    transaction,
                )?;
                Ok((Value::Text(upgraded.to_string()), None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "rollback", [Value::Text(target)]) => {
                let target = if let Ok(target) = target.parse::<ObjectId>() {
                    target
                } else {
                    self.require_package(current_state.subject, target)?
                };
                let rolled_back =
                    self.rollback_package(current_state.subject, object, target, transaction)?;
                self.stage_package_audit(
                    current_state.subject,
                    self.package_registry_object()?,
                    "rollback",
                    rolled_back,
                    Value::Record(BTreeMap::from([(
                        "from".to_owned(),
                        Value::Text(object.to_string()),
                    )])),
                    transaction,
                )?;
                Ok((Value::Text(rolled_back.to_string()), None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "data", []) => Ok((
                Value::Text(
                    self.package_data(current_state.subject, object, transaction)?
                        .to_string(),
                ),
                None,
            )),
            (CORE_PACKAGE_INSTALLATION_TYPE, "data_info", []) => {
                Ok((self.package_data_info(current_state.subject, object)?, None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "data_quota", []) => Ok((
                self.package_data_quota(current_state.subject, object)?,
                None,
            )),
            (CORE_PACKAGE_INSTALLATION_TYPE, "set_data_quota", [Value::Integer(quota)]) => {
                self.set_package_data_quota(current_state.subject, object, *quota, transaction)?;
                Ok((Value::Null, None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "reset_data_quota", []) => {
                self.reset_package_data_quota(current_state.subject, object, transaction)?;
                Ok((Value::Null, None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "data_export", []) => Ok((
                self.package_data_export(current_state.subject, object)?,
                None,
            )),
            (CORE_PACKAGE_INSTALLATION_TYPE, "data_import", [snapshot]) => {
                self.package_data_import(
                    current_state.subject,
                    object,
                    snapshot.clone(),
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "data_clear", []) => {
                self.package_data_import(current_state.subject, object, Value::Null, transaction)?;
                Ok((Value::Null, None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, "uninstall", []) => {
                let subject = current_state.subject;
                self.uninstall_package(
                    current_process,
                    current_state,
                    subject,
                    object,
                    transaction,
                )?;
                self.stage_package_audit(
                    subject,
                    self.package_registry_object()?,
                    "uninstall",
                    object,
                    Value::Record(BTreeMap::new()),
                    transaction,
                )?;
                Ok((Value::Null, None))
            }
            (CORE_PACKAGE_INSTALLATION_TYPE, export_name, arguments)
                if !matches!(
                    export_name,
                    "info"
                        | "verify"
                        | "module"
                        | "resource"
                        | "data"
                        | "data_info"
                        | "data_quota"
                        | "set_data_quota"
                        | "reset_data_quota"
                        | "data_export"
                        | "data_import"
                        | "data_clear"
                        | "run"
                        | "upgrade"
                        | "rollback"
                        | "uninstall"
                ) =>
            {
                let process = self.run_package_export(
                    current_state.subject,
                    object,
                    export_name,
                    arguments,
                    transaction,
                )?;
                self.stage_package_audit(
                    current_state.subject,
                    self.package_registry_object()?,
                    "export",
                    process,
                    Value::Record(BTreeMap::from([
                        ("installation".to_owned(), Value::Text(object.to_string())),
                        ("export".to_owned(), Value::Text(export_name.to_owned())),
                    ])),
                    transaction,
                )?;
                Ok((Value::Text(process.to_string()), None))
            }
            (CORE_PACKAGE_INSTANCE_TYPE, "process", []) => Ok((
                Value::Text(
                    self.package_instance_process(current_state.subject, object)?
                        .to_string(),
                ),
                None,
            )),
            (CORE_PACKAGE_INSTANCE_TYPE, "status", []) => Ok((
                self.package_instance_status(current_state.subject, object)?,
                None,
            )),
            (CORE_MODULE_REGISTRY_TYPE, "instances", [Value::Text(module)]) => {
                let module = module
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Module Object id"))?;
                Ok((self.module_instances(current_state.subject, module)?, None))
            }
            (CORE_MODULE_REGISTRY_TYPE, "enable", [Value::Text(module)]) => {
                let module = module
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Module Object id"))?;
                self.set_module_enabled(current_state.subject, module, true)?;
                Ok((Value::Null, None))
            }
            (CORE_MODULE_REGISTRY_TYPE, "disable", [Value::Text(module)]) => {
                let module = module
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Module Object id"))?;
                self.set_module_enabled(current_state.subject, module, false)?;
                Ok((Value::Null, None))
            }
            (
                CORE_MODULE_REGISTRY_TYPE,
                "upgrade",
                [
                    Value::Text(module),
                    Value::Text(version),
                    Value::Text(source),
                    Value::Array(capabilities),
                ],
            ) => {
                let module = module
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Module Object id"))?;
                let upgraded = self.upgrade_praxis_module(
                    current_state.subject,
                    module,
                    version,
                    source,
                    capabilities,
                    None,
                )?;
                Ok((Value::Text(upgraded.to_string()), None))
            }
            (
                CORE_MODULE_REGISTRY_TYPE,
                "upgrade",
                [
                    Value::Text(module),
                    Value::Text(version),
                    Value::Text(source),
                    Value::Array(capabilities),
                    Value::Array(dependencies),
                ],
            ) => {
                let module = module
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Module Object id"))?;
                let upgraded = self.upgrade_praxis_module(
                    current_state.subject,
                    module,
                    version,
                    source,
                    capabilities,
                    Some(dependencies),
                )?;
                Ok((Value::Text(upgraded.to_string()), None))
            }
            (
                CORE_MODULE_REGISTRY_TYPE,
                "rollback",
                [Value::Text(current), Value::Text(target)],
            ) => {
                let current = current
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid current Module Object id"))?;
                let target = target
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid rollback Module Object id"))?;
                self.rollback_module_version(current_state.subject, current, target)?;
                Ok((Value::Null, None))
            }
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
            (CORE_MODULE_REGISTRY_TYPE, "uninstall", [Value::Text(module)]) => {
                let module = module
                    .parse()
                    .map_err(|_| VmError::TypeError("invalid Module Object id"))?;
                let subject = current_state.subject;
                self.uninstall_module(
                    current_process,
                    current_state,
                    subject,
                    module,
                    transaction,
                )?;
                Ok((Value::Null, None))
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
            (CORE_CRYPTO_TYPE, "sha256", [value]) => {
                let bytes = match value {
                    Value::Text(text) => text.as_bytes().to_vec(),
                    Value::Bytes(bytes) => bytes.clone(),
                    value => value.encode()?,
                };
                Ok((
                    Value::Text(super::package_support::package_sha256(&bytes)),
                    None,
                ))
            }
            (CORE_MATH_TYPE, _, _) => Ok((math_capability(capability, arguments)?, None)),
            (CORE_VALUE_TYPE | CORE_TEXT_TYPE, _, _) => {
                let value = self.manager.value(self.context, object)?;
                Ok((text_capability(capability, arguments, &value)?, None))
            }
            (CORE_OBJECT_STORE_TYPE, "stats" | "health_check", []) => {
                if capability == "health_check" {
                    self.manager.health_check()?;
                }
                let stats = self.manager.stats()?;
                let performance = self.manager.performance_stats();
                let mut values = BTreeMap::from([
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
                    (
                        "commit_batches".to_owned(),
                        Value::Integer(counter_integer(performance.commit_batches)?),
                    ),
                    (
                        "transactions".to_owned(),
                        Value::Integer(counter_integer(performance.transactions)?),
                    ),
                    (
                        "persisted_batches".to_owned(),
                        Value::Integer(counter_integer(performance.persisted_batches)?),
                    ),
                    (
                        "delta_records".to_owned(),
                        Value::Integer(counter_integer(performance.delta_records)?),
                    ),
                    (
                        "delta_bytes".to_owned(),
                        Value::Integer(counter_integer(performance.delta_bytes)?),
                    ),
                    (
                        "snapshot_encodes".to_owned(),
                        Value::Integer(counter_integer(performance.full_snapshot_encodes)?),
                    ),
                    (
                        "snapshot_bytes".to_owned(),
                        Value::Integer(counter_integer(performance.full_snapshot_bytes)?),
                    ),
                    (
                        "commit_p50_ns".to_owned(),
                        Value::Integer(counter_integer(performance.commit_p50_nanos)?),
                    ),
                    (
                        "commit_p95_ns".to_owned(),
                        Value::Integer(counter_integer(performance.commit_p95_nanos)?),
                    ),
                    (
                        "commit_p99_ns".to_owned(),
                        Value::Integer(counter_integer(performance.commit_p99_nanos)?),
                    ),
                ]);
                if capability == "health_check" {
                    values.insert("healthy".to_owned(), Value::Bool(true));
                }
                Ok((Value::Record(values), None))
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
                let program = compile_program(source)
                    .map_err(|error| VmError::Provider(error.to_string()))?;
                let request = CreateObject::new(CORE_PROGRAM_TYPE, program.encode()?)
                    .with_parent(current_process);
                let program_id = request.id;
                let source_request =
                    CreateObject::new(CORE_TEXT_TYPE, Value::Text(source.clone()).encode()?)
                        .with_parent(program_id);
                let source_id = source_request.id;
                transaction
                    .create(request)
                    .create(source_request)
                    .set_link(program_id, "source", source_id);
                Ok((Value::Text(program_id.to_string()), None))
            }
            (CORE_COMPILER_TYPE, "validate", [Value::Text(source)]) => {
                Ok((Value::Bool(compile_program(source).is_ok()), None))
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
                Ok((self.wait_for_process(current_process, object)?, None))
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
}
