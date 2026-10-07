#![allow(clippy::wildcard_imports)]

use super::*;

const INSTALL_ROOT_MODULE: &str = "__ousject_install_root__";

impl VirtualMachine {
    pub(super) fn install_praxis_module(
        &self,
        subject: SubjectId,
        name: &str,
        version: &str,
        source: &str,
        requested_capabilities: &[Value],
        requested_dependencies: &[Value],
    ) -> Result<ObjectId, VmError> {
        if subject != SYSTEM_SUBJECT {
            return Err(VmError::TypeError("only local can install kernel modules"));
        }
        if !valid_module_name(name) || version.is_empty() || version.len() > 64 {
            return Err(VmError::TypeError("invalid module name or version"));
        }
        if source.len() > 1024 * 1024 {
            return Err(VmError::TypeError("module source exceeds 1 MiB"));
        }

        for header in self.manager.query(
            self.context,
            &ObjectQuery::new().with_type(CORE_MODULE_TYPE),
        )? {
            let Value::Record(fields) = self.manager.value(self.context, header.id)? else {
                continue;
            };
            if fields.get("name") == Some(&Value::Text(name.to_owned()))
                && fields.get("status") != Some(&Value::Text("superseded".to_owned()))
            {
                return Err(VmError::Provider(format!(
                    "module already installed: {name}"
                )));
            }
        }

        let capabilities = normalize_module_capabilities(requested_capabilities)?;

        let dependencies = self.module_dependency_ids(requested_dependencies, Some(name))?;
        let install_source = format!("import \"{INSTALL_ROOT_MODULE}\"");
        let program = compile_with_contextual_loader(&install_source, |requested, importer| {
            if requested == INSTALL_ROOT_MODULE && importer.is_none() {
                return Ok((source.to_owned(), INSTALL_ROOT_MODULE.to_owned()));
            }
            self.module_source_from_dependencies(requested, importer, &dependencies)
        })
        .map_err(|error| VmError::Provider(error.to_string()))?;
        let module = ObjectId::new();
        let program_id = ObjectId::new();
        let fields = BTreeMap::from([
            ("name".to_owned(), Value::Text(name.to_owned())),
            ("version".to_owned(), Value::Text(version.to_owned())),
            ("abi".to_owned(), Value::Integer(0)),
            ("kind".to_owned(), Value::Text("praxis".to_owned())),
            ("source".to_owned(), Value::Text(source.to_owned())),
            (
                "source_sha256".to_owned(),
                Value::Text(source_sha256(source)),
            ),
            ("program".to_owned(), Value::Text(program_id.to_string())),
            (
                "dependencies".to_owned(),
                Value::Array(
                    dependencies
                        .iter()
                        .map(|dependency| Value::Text(dependency.to_string()))
                        .collect(),
                ),
            ),
            (
                "capabilities".to_owned(),
                Value::Array(capabilities.into_iter().map(Value::Text).collect()),
            ),
            ("status".to_owned(), Value::Text("installed".to_owned())),
            ("installed_by".to_owned(), Value::Text(subject.to_string())),
        ]);
        let module_request =
            CreateObject::new(CORE_MODULE_TYPE, Value::Record(fields).encode()?).with_id(module);
        let program_request = CreateObject::new(PROGRAM_TYPE, program.encode()?)
            .with_id(program_id)
            .with_parent(module);
        let mut transaction = self.manager.begin(self.context);
        transaction.create(module_request).create(program_request);
        self.stage_dependent_links(&mut transaction, &dependencies, module)?;
        self.stage_audit_event(
            subject,
            "module.install",
            module,
            Value::Record(BTreeMap::from([
                ("name".to_owned(), Value::Text(name.to_owned())),
                ("version".to_owned(), Value::Text(version.to_owned())),
                (
                    "source_sha256".to_owned(),
                    Value::Text(source_sha256(source)),
                ),
            ])),
            &mut transaction,
        )?;
        self.manager.commit(transaction)?;
        Ok(module)
    }

    pub(super) fn installed_modules(&self) -> Result<Value, VmError> {
        Ok(Value::Array(
            self.manager
                .query(
                    self.context,
                    &ObjectQuery::new().with_type(CORE_MODULE_TYPE),
                )?
                .into_iter()
                .map(|module| Value::Text(module.id.to_string()))
                .collect(),
        ))
    }

    pub(super) fn find_praxis_module(
        &self,
        subject: SubjectId,
        name: &str,
        version: &str,
    ) -> Result<ObjectId, VmError> {
        if subject != SYSTEM_SUBJECT {
            return Err(VmError::TypeError(
                "only local can look up installed kernel modules",
            ));
        }
        if !valid_module_name(name) || version.is_empty() || version.len() > 64 {
            return Err(VmError::TypeError("invalid Module name or version"));
        }
        let mut found = None;
        for header in self.manager.query(
            self.context,
            &ObjectQuery::new().with_type(CORE_MODULE_TYPE),
        )? {
            let Value::Record(fields) = self.manager.value(self.context, header.id)? else {
                continue;
            };
            if fields.get("name") == Some(&Value::Text(name.to_owned()))
                && fields.get("version") == Some(&Value::Text(version.to_owned()))
                && fields.get("status") != Some(&Value::Text("retired".to_owned()))
                && found.replace(header.id).is_some()
            {
                return Err(VmError::Provider(format!(
                    "more than one Module is installed at {name}/{version}"
                )));
            }
        }
        found.ok_or_else(|| VmError::Provider(format!("Module not found: {name}/{version}")))
    }

    pub(super) fn module_instances(
        &self,
        subject: SubjectId,
        module: ObjectId,
    ) -> Result<Value, VmError> {
        if subject != SYSTEM_SUBJECT {
            return Err(VmError::TypeError(
                "only local can inspect Module Instances",
            ));
        }
        let module_view = self.manager.read(self.context, module)?;
        if module_view.header().type_id != CORE_MODULE_TYPE {
            return Err(VmError::TypeError("Object is not a Praxis Module"));
        }
        let mut instances = Vec::new();
        for (name, instance) in module_view.links() {
            if !name.starts_with("instance:") {
                continue;
            }
            let instance_view = self.manager.read(self.context, *instance)?;
            if instance_view.header().type_id != CORE_MODULE_INSTANCE_TYPE {
                continue;
            }
            let Value::Record(fields) = Value::decode(instance_view.state())? else {
                continue;
            };
            if fields.get("module") == Some(&Value::Text(module.to_string()))
                && fields.get("status") == Some(&Value::Text("active".to_owned()))
            {
                instances.push(Value::Text(instance.to_string()));
            }
        }
        Ok(Value::Array(instances))
    }

    pub(super) fn uninstall_module(
        &self,
        process: ObjectId,
        process_state: &mut ProcessState,
        subject: SubjectId,
        module: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        if subject != SYSTEM_SUBJECT {
            return Err(VmError::TypeError("only local can uninstall a Module"));
        }
        let view = self.manager.read(self.context, module)?;
        if view.header().type_id != CORE_MODULE_TYPE {
            return Err(VmError::TypeError("Object is not a Praxis Module"));
        }
        let Value::Record(mut fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Module state is not a Record"));
        };

        for (link_name, linked) in view.links() {
            if link_name.starts_with("instance:") {
                let instance_view = self.manager.read(self.context, *linked)?;
                if instance_view.header().type_id == CORE_MODULE_INSTANCE_TYPE
                    && matches!(
                        Value::decode(instance_view.state())?,
                        Value::Record(ref instance)
                            if instance.get("status") == Some(&Value::Text("active".to_owned()))
                    )
                {
                    return Err(VmError::Provider(
                        "Module is loaded in an active Terminal".to_owned(),
                    ));
                }
            } else if link_name.starts_with("dependent:") {
                let dependent_view = self.manager.read(self.context, *linked)?;
                if dependent_view.header().type_id == CORE_MODULE_TYPE {
                    let Value::Record(dependent) = Value::decode(dependent_view.state())? else {
                        continue;
                    };
                    return Err(VmError::Provider(format!(
                        "Module is required by installed version {}",
                        dependent
                            .get("name")
                            .and_then(|value| match value {
                                Value::Text(name) => Some(name.as_str()),
                                _ => None,
                            })
                            .unwrap_or("<unknown>"),
                    )));
                }
            }
        }

        fields.insert("status".to_owned(), Value::Text("retired".to_owned()));
        transaction
            .expect(module, view.header().version)
            .update_state(module, Value::Record(fields).encode()?);
        self.stage_audit_event(
            subject,
            "module.uninstall",
            module,
            Value::Record(BTreeMap::new()),
            transaction,
        )?;
        self.retire_object(process, process_state, module, transaction)
    }

    pub(super) fn set_module_enabled(
        &self,
        subject: SubjectId,
        module: ObjectId,
        enabled: bool,
    ) -> Result<(), VmError> {
        if subject != SYSTEM_SUBJECT {
            return Err(VmError::TypeError("only local can change module state"));
        }
        let view = self.manager.read(self.context, module)?;
        if view.header().type_id != CORE_MODULE_TYPE {
            return Err(VmError::TypeError("Object is not a Praxis Module"));
        }
        let Value::Record(mut fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Module state is not a Record"));
        };
        if fields.get("status") == Some(&Value::Text("superseded".to_owned())) {
            return Err(VmError::TypeError(
                "a superseded Module version cannot be enabled or disabled",
            ));
        }
        fields.insert(
            "status".to_owned(),
            Value::Text(if enabled { "enabled" } else { "disabled" }.to_owned()),
        );
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(module, view.header().version)
            .update_state(module, Value::Record(fields).encode()?);
        self.stage_audit_event(
            subject,
            if enabled {
                "module.enable"
            } else {
                "module.disable"
            },
            module,
            Value::Record(BTreeMap::new()),
            &mut transaction,
        )?;
        self.manager.commit(transaction)?;
        Ok(())
    }

    pub(super) fn upgrade_praxis_module(
        &self,
        subject: SubjectId,
        module: ObjectId,
        version: &str,
        source: &str,
        requested_capabilities: &[Value],
        requested_dependencies: Option<&[Value]>,
    ) -> Result<ObjectId, VmError> {
        if subject != SYSTEM_SUBJECT {
            return Err(VmError::TypeError("only local can upgrade modules"));
        }
        if version.is_empty() || version.len() > 64 || source.len() > 1024 * 1024 {
            return Err(VmError::TypeError("invalid Module version or source size"));
        }
        let capabilities = normalize_module_capabilities(requested_capabilities)?;
        let view = self.manager.read(self.context, module)?;
        if view.header().type_id != CORE_MODULE_TYPE {
            return Err(VmError::TypeError("Object is not a Praxis Module"));
        }
        let Value::Record(mut fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Module state is not a Record"));
        };
        let old_status = match fields.get("status") {
            Some(Value::Text(status)) if status != "superseded" => status.clone(),
            _ => {
                return Err(VmError::TypeError(
                    "superseded Module version cannot be upgraded",
                ));
            }
        };
        let old_name = record_text(&fields, "name")?;
        if record_text(&fields, "version")? == version {
            return Err(VmError::TypeError(
                "Module upgrade must use a different version",
            ));
        }
        self.ensure_module_version_available(module, old_name, version)?;
        let dependencies = match requested_dependencies {
            Some(values) => self.module_dependency_ids(values, Some(old_name))?,
            None => self.module_dependency_ids_from_record(&fields, Some(old_name))?,
        };
        let install_source = format!("import \"{INSTALL_ROOT_MODULE}\"");
        let program = compile_with_contextual_loader(&install_source, |requested, importer| {
            if requested == INSTALL_ROOT_MODULE && importer.is_none() {
                return Ok((source.to_owned(), INSTALL_ROOT_MODULE.to_owned()));
            }
            self.module_source_from_dependencies(requested, importer, &dependencies)
        })
        .map_err(|error| VmError::Provider(error.to_string()))?;
        let mut new_fields = fields.clone();
        let new_program = ObjectId::new();
        let new_module = ObjectId::new();
        new_fields.insert("version".to_owned(), Value::Text(version.to_owned()));
        new_fields.insert("source".to_owned(), Value::Text(source.to_owned()));
        new_fields.insert(
            "source_sha256".to_owned(),
            Value::Text(source_sha256(source)),
        );
        new_fields.insert("program".to_owned(), Value::Text(new_program.to_string()));
        new_fields.remove("superseded_from");
        new_fields.insert(
            "capabilities".to_owned(),
            Value::Array(capabilities.into_iter().map(Value::Text).collect()),
        );
        new_fields.insert(
            "dependencies".to_owned(),
            Value::Array(
                dependencies
                    .iter()
                    .map(|dependency| Value::Text(dependency.to_string()))
                    .collect(),
            ),
        );
        new_fields.insert("status".to_owned(), Value::Text(old_status.clone()));
        fields.insert("superseded_from".to_owned(), Value::Text(old_status));
        fields.insert("status".to_owned(), Value::Text("superseded".to_owned()));
        let module_request =
            CreateObject::new(CORE_MODULE_TYPE, Value::Record(new_fields).encode()?)
                .with_id(new_module);
        let program_request = CreateObject::new(PROGRAM_TYPE, program.encode()?)
            .with_id(new_program)
            .with_parent(new_module);
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(module, view.header().version)
            .update_state(module, Value::Record(fields).encode()?)
            .create(module_request)
            .create(program_request);
        self.stage_dependent_links(&mut transaction, &dependencies, new_module)?;
        self.stage_audit_event(
            subject,
            "module.upgrade",
            new_module,
            Value::Record(BTreeMap::from([
                ("previous".to_owned(), Value::Text(module.to_string())),
                ("version".to_owned(), Value::Text(version.to_owned())),
                (
                    "source_sha256".to_owned(),
                    Value::Text(source_sha256(source)),
                ),
            ])),
            &mut transaction,
        )?;
        self.manager.commit(transaction)?;
        Ok(new_module)
    }

    pub(super) fn rollback_module_version(
        &self,
        subject: SubjectId,
        current: ObjectId,
        target: ObjectId,
    ) -> Result<(), VmError> {
        if subject != SYSTEM_SUBJECT {
            return Err(VmError::TypeError("only local can roll back a Module"));
        }
        if current == target {
            return Err(VmError::TypeError("Module rollback target must differ"));
        }
        let current_view = self.manager.read(self.context, current)?;
        let target_view = self.manager.read(self.context, target)?;
        if current_view.header().type_id != CORE_MODULE_TYPE
            || target_view.header().type_id != CORE_MODULE_TYPE
        {
            return Err(VmError::TypeError(
                "Module rollback requires two Module Objects",
            ));
        }
        let Value::Record(mut current_fields) = Value::decode(current_view.state())? else {
            return Err(invalid_state("Current Module state is not a Record"));
        };
        let Value::Record(mut target_fields) = Value::decode(target_view.state())? else {
            return Err(invalid_state("Rollback target state is not a Record"));
        };
        if current_fields.get("name") != target_fields.get("name") {
            return Err(VmError::TypeError(
                "Module rollback requires matching names",
            ));
        }
        let current_status = match current_fields.get("status") {
            Some(Value::Text(status)) if status != "superseded" => status.clone(),
            _ => return Err(VmError::TypeError("current Module version is not active")),
        };
        let restore_status = match target_fields.get("superseded_from") {
            Some(Value::Text(status))
                if matches!(status.as_str(), "installed" | "enabled" | "disabled") =>
            {
                status.clone()
            }
            _ => return Err(VmError::TypeError("Module version has no rollback state")),
        };
        if target_fields.get("status") != Some(&Value::Text("superseded".to_owned())) {
            return Err(VmError::TypeError("rollback target is not superseded"));
        }
        current_fields.insert("status".to_owned(), Value::Text("superseded".to_owned()));
        current_fields.insert("superseded_from".to_owned(), Value::Text(current_status));
        target_fields.insert("status".to_owned(), Value::Text(restore_status));
        let mut transaction = self.manager.begin(self.context);
        transaction
            .expect(current, current_view.header().version)
            .expect(target, target_view.header().version)
            .update_state(current, Value::Record(current_fields).encode()?)
            .update_state(target, Value::Record(target_fields).encode()?);
        self.stage_audit_event(
            subject,
            "module.rollback",
            current,
            Value::Record(BTreeMap::from([(
                "restored".to_owned(),
                Value::Text(target.to_string()),
            )])),
            &mut transaction,
        )?;
        self.manager.commit(transaction)?;
        Ok(())
    }

    fn module_dependency_ids(
        &self,
        values: &[Value],
        forbidden_name: Option<&str>,
    ) -> Result<Vec<ObjectId>, VmError> {
        let mut ids = Vec::new();
        let mut names = BTreeSet::new();
        for value in values {
            let Value::Text(value) = value else {
                return Err(VmError::TypeError(
                    "module dependencies must be Object id Texts",
                ));
            };
            let id = value
                .parse()
                .map_err(|_| VmError::TypeError("invalid Module dependency Object id"))?;
            if ids.contains(&id) {
                return Err(VmError::TypeError("duplicate Module dependency"));
            }
            let dependency = self.manager.read(self.context, id)?;
            if dependency.header().type_id != CORE_MODULE_TYPE {
                return Err(VmError::TypeError("dependency is not a Praxis Module"));
            }
            let Value::Record(fields) = Value::decode(dependency.state())? else {
                return Err(invalid_state("Module dependency state is not a Record"));
            };
            let name = record_text(&fields, "name")?;
            if forbidden_name == Some(name) {
                return Err(VmError::TypeError("a Module cannot depend on its own name"));
            }
            if !names.insert(name.to_owned()) {
                return Err(VmError::TypeError(
                    "Module dependencies cannot contain multiple versions of one name",
                ));
            }
            if !module_version_is_importable(&fields) {
                return Err(VmError::TypeError(
                    "Module dependency must be enabled or a retained enabled version",
                ));
            }
            ids.push(id);
        }
        Ok(ids)
    }

    fn module_dependency_ids_from_record(
        &self,
        fields: &BTreeMap<String, Value>,
        forbidden_name: Option<&str>,
    ) -> Result<Vec<ObjectId>, VmError> {
        let Some(Value::Array(values)) = fields.get("dependencies") else {
            return Err(invalid_state("Module dependencies are malformed"));
        };
        self.module_dependency_ids(values, forbidden_name)
    }

    fn stage_dependent_links(
        &self,
        transaction: &mut Transaction,
        dependencies: &[ObjectId],
        dependent: ObjectId,
    ) -> Result<(), VmError> {
        for dependency in dependencies {
            let view = self.manager.read(self.context, *dependency)?;
            transaction
                .expect(*dependency, view.header().version)
                .set_link(*dependency, format!("dependent:{dependent}"), dependent);
        }
        Ok(())
    }

    fn module_source_from_dependencies(
        &self,
        name: &str,
        importer: Option<&str>,
        install_dependencies: &[ObjectId],
    ) -> Result<(String, String), String> {
        let dependencies = if importer == Some(INSTALL_ROOT_MODULE) {
            install_dependencies.to_vec()
        } else if let Some(importer) = importer {
            let importer = importer
                .parse::<ObjectId>()
                .map_err(|_| "invalid importing Module identity".to_owned())?;
            let state = self
                .manager
                .value(self.context, importer)
                .map_err(|error| error.to_string())?;
            let Value::Record(fields) = state else {
                return Err("importing Module state is malformed".to_owned());
            };
            let Some(Value::Array(dependencies)) = fields.get("dependencies") else {
                return Err("importing Module dependencies are malformed".to_owned());
            };
            dependencies
                .iter()
                .map(|value| match value {
                    Value::Text(id) => id
                        .parse::<ObjectId>()
                        .map_err(|_| "invalid Module dependency Object id".to_owned()),
                    _ => Err("Module dependencies must be Object id Texts".to_owned()),
                })
                .collect::<Result<Vec<_>, _>>()?
        } else {
            let context = AccessContext::new(SYSTEM_SUBJECT);
            let modules = self
                .manager
                .query(context, &ObjectQuery::new().with_type(CORE_MODULE_TYPE))
                .map_err(|error| error.to_string())?;
            for header in modules {
                let state = self
                    .manager
                    .value(context, header.id)
                    .map_err(|error| error.to_string())?;
                let Value::Record(fields) = state else {
                    continue;
                };
                if fields.get("name") == Some(&Value::Text(name.to_owned()))
                    && fields.get("status") == Some(&Value::Text("enabled".to_owned()))
                {
                    let source = checked_module_source(&fields)?;
                    return Ok((source, header.id.to_string()));
                }
            }
            return Err(format!("module is not installed and enabled: {name}"));
        };

        let context = AccessContext::new(SYSTEM_SUBJECT);
        for id in dependencies {
            let state = self
                .manager
                .value(context, id)
                .map_err(|error| error.to_string())?;
            let Value::Record(fields) = state else {
                continue;
            };
            if fields.get("name") != Some(&Value::Text(name.to_owned())) {
                continue;
            }
            if !module_version_is_importable(&fields) {
                return Err(format!("locked dependency is not enabled: {name}"));
            }
            let source = checked_module_source(&fields)?;
            return Ok((source, id.to_string()));
        }
        Err(format!(
            "importing Module did not declare dependency: {name}"
        ))
    }

    pub(super) fn module_source(
        &self,
        name: &str,
        importer: Option<&str>,
    ) -> Result<(String, String), String> {
        self.module_source_from_dependencies(name, importer, &[])
    }

    fn ensure_module_version_available(
        &self,
        current: ObjectId,
        name: &str,
        version: &str,
    ) -> Result<(), VmError> {
        for candidate in self.manager.query(
            self.context,
            &ObjectQuery::new().with_type(CORE_MODULE_TYPE),
        )? {
            if candidate.id == current {
                continue;
            }
            let Value::Record(fields) = self.manager.value(self.context, candidate.id)? else {
                continue;
            };
            if fields.get("name") == Some(&Value::Text(name.to_owned()))
                && fields.get("version") == Some(&Value::Text(version.to_owned()))
                && fields.get("status") != Some(&Value::Text("retired".to_owned()))
            {
                return Err(VmError::Provider(format!(
                    "Module version already exists: {name}/{version}"
                )));
            }
        }
        Ok(())
    }
}

fn normalize_module_capabilities(values: &[Value]) -> Result<BTreeSet<String>, VmError> {
    values
        .iter()
        .map(|value| {
            let Value::Text(capability) = value else {
                return Err(VmError::TypeError("module capabilities must be Text"));
            };
            if capability.is_empty() || capability.len() > 128 {
                return Err(VmError::TypeError("invalid module capability name"));
            }
            Ok(capability.clone())
        })
        .collect()
}

fn record_text<'a>(fields: &'a BTreeMap<String, Value>, key: &str) -> Result<&'a str, VmError> {
    match fields.get(key) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(invalid_state("Module text metadata is malformed")),
    }
}

fn module_version_is_importable(fields: &BTreeMap<String, Value>) -> bool {
    fields.get("status") == Some(&Value::Text("enabled".to_owned()))
        || (fields.get("status") == Some(&Value::Text("superseded".to_owned()))
            && fields.get("superseded_from") == Some(&Value::Text("enabled".to_owned())))
}

pub(super) fn checked_module_source(fields: &BTreeMap<String, Value>) -> Result<String, String> {
    let source = record_text(fields, "source").map_err(|error| error.to_string())?;
    let expected = record_text(fields, "source_sha256").map_err(|error| error.to_string())?;
    if expected != source_sha256(source) {
        return Err("Module source SHA-256 mismatch".to_owned());
    }
    Ok(source.to_owned())
}

pub(super) fn source_sha256(source: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in ousject_auth::sha256_digest(source.as_bytes()) {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}

fn valid_module_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}
