#![allow(clippy::wildcard_imports)]

use super::package_support::*;
use super::*;
use praxis_compiler::{compile_program_with_contextual_loader, compile_with_contextual_loader};

impl VirtualMachine {
    pub(super) fn build_package_artifact(
        &self,
        subject: SubjectId,
        specification: &Value,
        registry: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        if subject != SYSTEM_SUBJECT {
            return Err(VmError::TypeError(
                "only local can build and publish Packages",
            ));
        }
        let registry_header = self
            .manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), registry)?;
        if registry_header.type_id != CORE_PACKAGE_REGISTRY_TYPE {
            return Err(VmError::TypeError("Object is not the Package Registry"));
        }
        let fields = value_record(specification, "Package specification")?;
        if fields.keys().any(|field| {
            !matches!(
                field.as_str(),
                "namespace"
                    | "name"
                    | "version"
                    | "dependencies"
                    | "modules"
                    | "entry"
                    | "kind"
                    | "capabilities"
                    | "exports"
                    | "resources"
            )
        }) {
            return Err(VmError::TypeError(
                "Package specification contains an unknown field",
            ));
        }
        let namespace = required_text(fields, "namespace")?;
        let name = required_text(fields, "name")?;
        let version = required_text(fields, "version")?;
        validate_coordinate(namespace, "namespace")?;
        validate_coordinate(name, "name")?;
        validate_release(version)?;
        let coordinate = format!("{namespace}/{name}/{version}");
        let dependencies = self.lock_package_dependencies(
            subject,
            registry,
            fields.get("dependencies"),
            &coordinate,
        )?;
        let dependency_sources = self.package_dependency_sources(&dependencies)?;

        let module_values = fields.get("modules").ok_or(VmError::TypeError(
            "Package specification is missing modules",
        ))?;
        let module_fields = match module_values {
            Value::Map(fields) | Value::Record(fields) => fields,
            _ => return Err(VmError::TypeError("Package modules must be a Map")),
        };
        if module_fields.is_empty() || module_fields.len() > 256 {
            return Err(VmError::TypeError(
                "Package must contain between 1 and 256 modules",
            ));
        }
        let mut sources = BTreeMap::new();
        let mut visited_modules = BTreeSet::new();
        let mut visiting_modules = BTreeSet::new();
        for (module_name, module_value) in module_fields {
            if !valid_package_module_name(module_name) {
                return Err(VmError::TypeError("invalid Package module name"));
            }
            let Value::Text(module_id) = module_value else {
                return Err(VmError::TypeError(
                    "Package modules must contain Module Object IDs; use modules.find(name, version)",
                ));
            };
            let module_id = module_id.parse::<ObjectId>().map_err(|_| {
                VmError::TypeError("Package module value is not a Module Object ID")
            })?;
            let actual_name = self.collect_package_module_sources(
                module_id,
                &mut sources,
                &mut visited_modules,
                &mut visiting_modules,
            )?;
            if actual_name != *module_name {
                return Err(VmError::TypeError(
                    "Package module key must match the Module Object name",
                ));
            }
        }
        if sources.len() > 256 {
            return Err(VmError::TypeError(
                "Package and bundled Module dependencies exceed 256 modules",
            ));
        }
        let mut total_source_bytes = 0_usize;
        for source in sources.values() {
            if source.len() > MAX_PACKAGE_MODULE_BYTES {
                return Err(VmError::TypeError("Package module source exceeds 1 MiB"));
            }
            total_source_bytes = total_source_bytes
                .checked_add(source.len())
                .ok_or(VmError::TypeError("Package source is too large"))?;
            if total_source_bytes > MAX_PACKAGE_SOURCE_BYTES {
                return Err(VmError::TypeError(
                    "Package sources exceed the 8 MiB build limit",
                ));
            }
        }

        let entry_program_id = match fields.get("entry") {
            Some(Value::Text(id)) => Some(
                id.parse::<ObjectId>()
                    .map_err(|_| VmError::TypeError("Package entry must be a Program Object ID"))?,
            ),
            Some(Value::Null) | None => None,
            _ => {
                return Err(VmError::TypeError(
                    "Package entry must be a Program Object ID",
                ));
            }
        };
        let entry_source = entry_program_id
            .map(|program| self.package_entry_source(program))
            .transpose()?;
        let inferred_kind = if entry_source.is_some() {
            "application"
        } else {
            "library"
        };
        let kind = match fields.get("kind") {
            Some(Value::Text(kind)) => kind.as_str(),
            None | Some(Value::Null) => inferred_kind,
            _ => return Err(VmError::TypeError("Package kind must be Text")),
        };
        if !matches!(kind, "library" | "application") {
            return Err(VmError::TypeError(
                "Package kind must be library or application",
            ));
        }
        match (kind, entry_source.is_some()) {
            ("application", false) => {
                return Err(VmError::TypeError(
                    "application Package requires an entry Program Object",
                ));
            }
            ("library", true) => {
                return Err(VmError::TypeError(
                    "library Package cannot contain an application entry",
                ));
            }
            _ => {}
        }
        if let Some((source, _)) = &entry_source {
            if source.len() > MAX_PACKAGE_MODULE_BYTES {
                return Err(VmError::TypeError("Package entry source exceeds 1 MiB"));
            }
            total_source_bytes = total_source_bytes
                .checked_add(source.len())
                .ok_or(VmError::TypeError("Package source is too large"))?;
            if total_source_bytes > MAX_PACKAGE_SOURCE_BYTES {
                return Err(VmError::TypeError(
                    "Package sources exceed the 8 MiB build limit",
                ));
            }
        }

        let capabilities = normalize_capabilities(fields.get("capabilities"))?;
        let exports = normalize_package_exports(fields.get("exports"))?;
        let loader_sources = sources.clone();
        let mut encoded_modules = BTreeMap::new();
        let mut expanded_source_bytes = 0_usize;
        let mut total_compiled_bytes = 0_usize;
        for (module, source) in &sources {
            let loader_sources = loader_sources.clone();
            let loader_dependencies = dependency_sources.clone();
            let wrapper = format!("import \"{module}\"");
            let program = compile_with_contextual_loader(&wrapper, |requested, importer| {
                let (source, identity) = package_compile_source(
                    requested,
                    importer,
                    &loader_sources,
                    &loader_dependencies,
                )?;
                expanded_source_bytes = expanded_source_bytes.saturating_add(source.len());
                if expanded_source_bytes > MAX_PACKAGE_EXPANDED_SOURCE_BYTES {
                    return Err("Package compilation expansion exceeds 64 MiB".to_owned());
                }
                Ok((source, identity))
            })
            .map_err(|error| VmError::Provider(error.to_string()))?;
            let encoded_program = program.encode().map_err(VmError::from)?;
            total_compiled_bytes = total_compiled_bytes.saturating_add(encoded_program.len());
            if total_compiled_bytes > MAX_PACKAGE_COMPILED_BYTES {
                return Err(VmError::TypeError(
                    "Package compiled programs exceed the 32 MiB build limit",
                ));
            }
            encoded_modules.insert(
                module.clone(),
                Value::Record(BTreeMap::from([
                    ("source".to_owned(), Value::Text(source.clone())),
                    (
                        "source_sha256".to_owned(),
                        Value::Text(package_sha256(source.as_bytes())),
                    ),
                    ("program".to_owned(), Value::Bytes(encoded_program)),
                ])),
            );
        }

        let Value::Record(export_fields) = &exports else {
            return Err(invalid_state("normalized Package exports are malformed"));
        };
        for (export_name, export_value) in export_fields {
            let Value::Record(export) = export_value else {
                return Err(invalid_state("normalized Package export is malformed"));
            };
            let (
                Some(Value::Text(module)),
                Some(Value::Text(function)),
                Some(Value::Integer(arity)),
            ) = (
                export.get("module"),
                export.get("function"),
                export.get("arguments"),
            )
            else {
                return Err(invalid_state(
                    "normalized Package export fields are malformed",
                ));
            };
            let Some(Value::Record(module_record)) = encoded_modules.get(module) else {
                return Err(VmError::TypeError(
                    "Package export refers to a missing Module",
                ));
            };
            let Some(Value::Bytes(program)) = module_record.get("program") else {
                return Err(invalid_state("Package Module has no compiled Program"));
            };
            let program = Program::decode(program)?;
            let arity = usize::try_from(*arity)
                .map_err(|_| VmError::TypeError("Package export arity is invalid"))?;
            super::find_function(&program, function, arity).map_err(|_| {
                VmError::Provider(format!(
                    "Package export '{export_name}' does not match a Module function"
                ))
            })?;
        }

        let entry = if let Some((source, original_program)) = &entry_source {
            let loader_sources = sources.clone();
            let loader_dependencies = dependency_sources.clone();
            let program = compile_program_with_contextual_loader(source, |requested, importer| {
                let (source, identity) = package_compile_source(
                    requested,
                    importer,
                    &loader_sources,
                    &loader_dependencies,
                )?;
                expanded_source_bytes = expanded_source_bytes.saturating_add(source.len());
                if expanded_source_bytes > MAX_PACKAGE_EXPANDED_SOURCE_BYTES {
                    return Err("Package compilation expansion exceeds 64 MiB".to_owned());
                }
                Ok((source, identity))
            })
            .map_err(|error| VmError::Provider(error.to_string()))?;
            let encoded_program = program.encode().map_err(VmError::from)?;
            if encoded_program != *original_program {
                return Err(VmError::Provider(
                    "Package entry Program does not match its saved source".to_owned(),
                ));
            }
            total_compiled_bytes = total_compiled_bytes.saturating_add(encoded_program.len());
            if total_compiled_bytes > MAX_PACKAGE_COMPILED_BYTES {
                return Err(VmError::TypeError(
                    "Package compiled programs exceed the 32 MiB build limit",
                ));
            }
            Value::Record(BTreeMap::from([
                ("source".to_owned(), Value::Text(source.to_owned())),
                (
                    "source_sha256".to_owned(),
                    Value::Text(package_sha256(source.as_bytes())),
                ),
                ("program".to_owned(), Value::Bytes(encoded_program)),
            ]))
        } else {
            Value::Null
        };

        let manifest = Value::Record(BTreeMap::from([
            ("format_version".to_owned(), Value::Integer(0)),
            ("praxis_language".to_owned(), Value::Integer(0)),
            ("namespace".to_owned(), Value::Text(namespace.to_owned())),
            ("name".to_owned(), Value::Text(name.to_owned())),
            ("version".to_owned(), Value::Text(version.to_owned())),
            ("kind".to_owned(), Value::Text(kind.to_owned())),
            ("modules".to_owned(), Value::Record(encoded_modules)),
            ("entry".to_owned(), entry),
            (
                "resources".to_owned(),
                self.package_resource_snapshot(fields.get("resources"))?,
            ),
            ("exports".to_owned(), exports),
            (
                "capabilities".to_owned(),
                Value::Array(capabilities.into_iter().map(Value::Text).collect()),
            ),
            ("dependencies".to_owned(), dependencies),
        ]));
        let artifact_hash = package_sha256(&manifest.encode()?);

        let registry_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), registry)?;
        let coordinate_key = format!("coordinate:{namespace}/{name}/{version}");
        let hash_key = format!("package:{artifact_hash}");
        if let Some(existing) = registry_view.links().get(&coordinate_key).copied() {
            let existing_hash = artifact_manifest_hash(
                &self
                    .manager
                    .value(AccessContext::new(SYSTEM_SUBJECT), existing)?,
            )?;
            if existing_hash == artifact_hash && self.verify_package_artifact(existing)? {
                return Ok(existing);
            }
            return Err(VmError::Provider(
                "Package coordinate is already bound to different content".to_owned(),
            ));
        }
        if let Some(existing) = registry_view.links().get(&hash_key).copied() {
            if !self.verify_package_artifact(existing)?
                || artifact_manifest_hash(
                    &self
                        .manager
                        .value(AccessContext::new(SYSTEM_SUBJECT), existing)?,
                )? != artifact_hash
            {
                return Err(invalid_state("Package hash index is inconsistent"));
            }
            return Err(invalid_state(
                "Package hash is bound to a different coordinate",
            ));
        }

        let artifact = ObjectId::new();
        let value = Value::Record(BTreeMap::from([
            ("manifest".to_owned(), manifest),
            ("sha256".to_owned(), Value::Text(artifact_hash)),
            ("status".to_owned(), Value::Text("verified".to_owned())),
        ]));
        let mut request = CreateObject::new(CORE_PACKAGE_TYPE, value.encode()?)
            .with_id(artifact)
            .with_parent(registry);
        request.capabilities = [Capability::Inspect, Capability::ViewValue]
            .into_iter()
            .collect();
        transaction
            .expect(registry, registry_view.header().version)
            .create(request)
            .set_link(registry, hash_key, artifact)
            .set_link(registry, coordinate_key, artifact);
        Ok(artifact)
    }

    fn package_entry_source(&self, program: ObjectId) -> Result<(String, Vec<u8>), VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let program_view = self.manager.read(system, program)?;
        if program_view.header().type_id != CORE_PROGRAM_TYPE {
            return Err(VmError::TypeError("Package entry is not a Program Object"));
        }
        let program = Program::decode(program_view.state())?;
        super::find_function(&program, "main", 0).map_err(|_| {
            VmError::TypeError("Package Application entry must define main() with no arguments")
        })?;
        let source = program_view
            .links()
            .get("source")
            .copied()
            .ok_or(VmError::TypeError(
                "Package entry Program has no saved source; compile it with compiler.compile()",
            ))?;
        let source_view = self.manager.read(system, source)?;
        if source_view.header().type_id != CORE_TEXT_TYPE {
            return Err(invalid_state("Program source Link does not point to Text"));
        }
        let Value::Text(source) = Value::decode(source_view.state())? else {
            return Err(invalid_state("Program source Object is not Text"));
        };
        Ok((source, program_view.state().to_vec()))
    }

    fn collect_package_module_sources(
        &self,
        module_id: ObjectId,
        sources: &mut BTreeMap<String, String>,
        visited: &mut BTreeSet<ObjectId>,
        visiting: &mut BTreeSet<ObjectId>,
    ) -> Result<String, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, module_id)?;
        if view.header().type_id != CORE_MODULE_TYPE {
            return Err(VmError::TypeError("Package input is not a Praxis Module"));
        }
        let Value::Record(fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Praxis Module state is malformed"));
        };
        let module_name = match fields.get("name") {
            Some(Value::Text(name)) if valid_package_module_name(name) => name.clone(),
            _ => return Err(invalid_state("Praxis Module has an invalid name")),
        };
        if fields.get("status") == Some(&Value::Text("retired".to_owned())) {
            return Err(VmError::TypeError("a retired Module cannot be packaged"));
        }
        let source = super::modules::checked_module_source(&fields).map_err(VmError::Provider)?;
        if let Some(existing) = sources.get(&module_name) {
            if existing != &source {
                return Err(VmError::TypeError(
                    "two Module Objects with the same name contain different source",
                ));
            }
        } else {
            sources.insert(module_name.clone(), source);
        }
        if visited.contains(&module_id) {
            return Ok(module_name);
        }
        if !visiting.insert(module_id) {
            return Err(VmError::TypeError("cyclic Module dependency graph"));
        }
        let dependencies = match fields.get("dependencies") {
            Some(Value::Array(dependencies)) => dependencies,
            None => return Err(invalid_state("Praxis Module has no dependency list")),
            _ => return Err(invalid_state("Praxis Module dependencies are malformed")),
        };
        for dependency in dependencies {
            let Value::Text(dependency) = dependency else {
                return Err(invalid_state("Praxis Module dependency ID is malformed"));
            };
            let dependency = dependency
                .parse::<ObjectId>()
                .map_err(|_| invalid_state("Praxis Module dependency ID is malformed"))?;
            self.collect_package_module_sources(dependency, sources, visited, visiting)?;
            if sources.len() > 256 {
                return Err(VmError::TypeError(
                    "Package and bundled Module dependencies exceed 256 modules",
                ));
            }
        }
        visiting.remove(&module_id);
        visited.insert(module_id);
        Ok(module_name)
    }

    fn package_resource_snapshot(&self, value: Option<&Value>) -> Result<Value, VmError> {
        let resource_values = match value {
            Some(Value::Record(resources) | Value::Map(resources)) => resources,
            None | Some(Value::Null) => return Ok(Value::Record(BTreeMap::new())),
            _ => return Err(VmError::TypeError("Package resources must be a Map")),
        };
        if resource_values.len() > 256 {
            return Err(VmError::TypeError("Package has more than 256 resources"));
        }
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let mut resources = BTreeMap::new();
        let mut total_bytes = 0_usize;
        for (name, value) in resource_values {
            if !valid_package_module_name(name) {
                return Err(VmError::TypeError("invalid Package resource name"));
            }
            let snapshot = if let Value::Text(candidate) = value {
                if let Ok(object) = candidate.parse::<ObjectId>() {
                    self.manager.value(system, object)?
                } else {
                    value.clone()
                }
            } else {
                value.clone()
            };
            total_bytes = total_bytes
                .checked_add(snapshot.encode()?.len())
                .ok_or(VmError::TypeError("Package resources are too large"))?;
            if total_bytes > MAX_PACKAGE_SOURCE_BYTES {
                return Err(VmError::TypeError(
                    "Package resources exceed the 8 MiB build limit",
                ));
            }
            resources.insert(name.clone(), snapshot);
        }
        Ok(Value::Record(resources))
    }

    fn package_dependency_sources(
        &self,
        value: &Value,
    ) -> Result<BTreeMap<String, String>, VmError> {
        let dependencies = parse_package_dependencies(Some(value))?;
        if dependencies.is_empty() {
            return Ok(BTreeMap::new());
        }
        let registry = self.package_registry_object()?;
        let registry_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), registry)?;
        let mut visiting = BTreeSet::new();
        let mut visited = BTreeSet::new();
        let mut sources = BTreeMap::new();
        let mut total_source_bytes = 0_usize;
        for dependency in &dependencies {
            self.collect_dependency_sources(
                &registry_view,
                dependency,
                1,
                &mut visiting,
                &mut visited,
                &mut sources,
                &mut total_source_bytes,
            )?;
        }
        Ok(sources)
    }

    fn collect_dependency_sources(
        &self,
        registry: &ObjectView,
        expected: &PackageDependency,
        depth: usize,
        visiting: &mut BTreeSet<String>,
        visited: &mut BTreeSet<String>,
        sources: &mut BTreeMap<String, String>,
        total_source_bytes: &mut usize,
    ) -> Result<(), VmError> {
        if visited.contains(&expected.coordinate) {
            return Ok(());
        }
        if depth > MAX_PACKAGE_DEPENDENCY_DEPTH {
            return Err(VmError::TypeError("Package dependency depth exceeds 64"));
        }
        if !visiting.insert(expected.coordinate.clone()) {
            return Err(VmError::TypeError("Package dependency cycle detected"));
        }
        if visiting.len() + visited.len() > MAX_PACKAGE_DEPENDENCIES {
            return Err(VmError::TypeError(
                "Package dependency closure exceeds 256 Packages",
            ));
        }
        let artifact = registry
            .links()
            .get(&format!("coordinate:{}", expected.coordinate))
            .copied()
            .ok_or_else(|| VmError::MissingKey(expected.coordinate.clone()))?;
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, artifact)?;
        if view.header().type_id != CORE_PACKAGE_TYPE {
            return Err(VmError::TypeError("Package dependency is not an Package"));
        }
        let Value::Record(fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Package dependency Package is malformed"));
        };
        if fields.get("sha256") != Some(&Value::Text(expected.sha256.clone()))
            || artifact_manifest_hash(&Value::Record(fields.clone()))? != expected.sha256
            || registry
                .links()
                .get(&format!("package:{}", expected.sha256))
                != Some(&artifact)
        {
            return Err(VmError::TypeError(
                "Package dependency SHA-256 is inconsistent",
            ));
        }
        let Some(Value::Record(manifest)) = fields.get("manifest") else {
            return Err(invalid_state("Package dependency has no Manifest"));
        };
        if package_coordinate(manifest)? != expected.coordinate {
            return Err(VmError::TypeError(
                "Package dependency coordinate is inconsistent",
            ));
        }
        let dependencies = parse_package_dependencies(manifest.get("dependencies"))?;
        let Some(Value::Record(modules)) = manifest.get("modules") else {
            return Err(invalid_state("Package dependency modules are malformed"));
        };
        if modules.len() > 256 {
            return Err(VmError::TypeError(
                "Package dependency has more than 256 Modules",
            ));
        }
        for (module_name, value) in modules {
            if !valid_package_module_name(module_name) {
                return Err(invalid_state(
                    "Package dependency has an invalid Module name",
                ));
            }
            let Value::Record(module) = value else {
                return Err(invalid_state("Package dependency Module is malformed"));
            };
            let (Some(Value::Text(source)), Some(Value::Text(source_hash))) =
                (module.get("source"), module.get("source_sha256"))
            else {
                return Err(invalid_state(
                    "Package dependency Module source is malformed",
                ));
            };
            *total_source_bytes =
                total_source_bytes
                    .checked_add(source.len())
                    .ok_or(VmError::TypeError(
                        "Package dependency sources are too large",
                    ))?;
            if package_sha256(source.as_bytes()) != *source_hash
                || source.len() > MAX_PACKAGE_MODULE_BYTES
                || *total_source_bytes > MAX_PACKAGE_SOURCE_BYTES
                || sources.len() >= 256
            {
                return Err(VmError::TypeError(
                    "Package dependency sources exceed their validation limits",
                ));
            }
            let identity = package_module_import_name(&expected.coordinate, module_name);
            if sources.insert(identity, source.clone()).is_some() {
                return Err(VmError::TypeError("duplicate Package dependency module"));
            }
        }
        for dependency in &dependencies {
            self.collect_dependency_sources(
                registry,
                dependency,
                depth + 1,
                visiting,
                visited,
                sources,
                total_source_bytes,
            )?;
        }
        visiting.remove(&expected.coordinate);
        visited.insert(expected.coordinate.clone());
        Ok(())
    }

    fn lock_package_dependencies(
        &self,
        owner: SubjectId,
        registry: ObjectId,
        value: Option<&Value>,
        self_coordinate: &str,
    ) -> Result<Value, VmError> {
        let Some(value) = value else {
            return Ok(Value::Array(Vec::new()));
        };
        let Value::Array(references) = value else {
            return Err(VmError::TypeError(
                "Package dependencies must be an Array of Object IDs",
            ));
        };
        if references.len() > MAX_PACKAGE_DEPENDENCIES {
            return Err(VmError::TypeError("Package has more than 256 dependencies"));
        }
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let registry_view = self.manager.read(system, registry)?;
        let mut dependencies = BTreeMap::<String, PackageDependency>::new();
        for reference in references {
            let Value::Text(id) = reference else {
                return Err(VmError::TypeError(
                    "Package dependency must be an Object ID Text",
                ));
            };
            let object = id
                .parse::<ObjectId>()
                .map_err(|_| VmError::TypeError("Package dependency is not an Object ID"))?;
            let object_view = self.manager.read(system, object)?;
            let artifact = match object_view.header().type_id {
                CORE_PACKAGE_TYPE => object,
                CORE_PACKAGE_INSTALLATION_TYPE => {
                    let value = Value::decode(object_view.state())?;
                    if !package_installation_owned(&value, owner)
                        || !self.verify_package_installation(owner, object)?
                    {
                        return Err(VmError::TypeError(
                            "Package dependency Installation is invalid or belongs to another user",
                        ));
                    }
                    match value_record(&value, "Package Installation")?.get("package") {
                        Some(Value::Text(id)) => id.parse::<ObjectId>().map_err(|_| {
                            invalid_state("Package dependency Package ID is malformed")
                        })?,
                        _ => return Err(invalid_state("Package dependency has no Package")),
                    }
                }
                _ => {
                    return Err(VmError::TypeError(
                        "Package dependency must be an Package or Installation Object",
                    ));
                }
            };
            if !self.verify_package_artifact(artifact)? {
                return Err(VmError::TypeError("Package dependency Package is invalid"));
            }
            let artifact_value = self.manager.value(system, artifact)?;
            let Value::Record(artifact_fields) = artifact_value else {
                return Err(invalid_state("Package dependency Package is malformed"));
            };
            let hash = match artifact_fields.get("sha256") {
                Some(Value::Text(hash)) => hash.clone(),
                _ => return Err(invalid_state("Package dependency Package has no SHA-256")),
            };
            if artifact_manifest_hash(&Value::Record(artifact_fields.clone()))? != hash {
                return Err(VmError::TypeError(
                    "Package dependency Package hash is invalid",
                ));
            }
            let manifest = match artifact_fields.get("manifest") {
                Some(Value::Record(manifest)) => manifest,
                _ => return Err(invalid_state("Package dependency Manifest is malformed")),
            };
            let coordinate = package_coordinate(manifest)?;
            if coordinate == self_coordinate {
                return Err(VmError::TypeError(
                    "Package cannot depend on its own coordinate",
                ));
            }
            if registry_view
                .links()
                .get(&format!("coordinate:{coordinate}"))
                != Some(&artifact)
                || registry_view.links().get(&format!("package:{hash}")) != Some(&artifact)
            {
                return Err(VmError::TypeError(
                    "Package dependency is not registered under its exact coordinate and SHA-256",
                ));
            }
            let dependency = PackageDependency {
                coordinate: coordinate.clone(),
                sha256: hash,
            };
            if let Some(previous) = dependencies.insert(coordinate, dependency.clone()) {
                if previous.sha256 != dependency.sha256 {
                    return Err(VmError::TypeError(
                        "Package dependency coordinate resolves to conflicting content",
                    ));
                }
            }
        }
        Ok(package_dependencies_value(
            &dependencies.into_values().collect::<Vec<_>>(),
        ))
    }

    pub(super) fn package_artifact_info(&self, package: ObjectId) -> Result<Value, VmError> {
        let view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), package)?;
        if view.header().type_id != CORE_PACKAGE_TYPE {
            return Err(VmError::TypeError("Object is not a Package"));
        }
        let value = Value::decode(view.state())?;
        let Value::Record(fields) = value else {
            return Err(invalid_state("Package is malformed"));
        };
        let manifest =
            match fields.get("manifest") {
                Some(Value::Record(manifest)) => {
                    Value::Record(BTreeMap::from([
                        (
                            "format_version".to_owned(),
                            manifest.get("format_version").cloned().ok_or_else(|| {
                                invalid_state("Package Manifest has no format version")
                            })?,
                        ),
                        (
                            "namespace".to_owned(),
                            manifest.get("namespace").cloned().ok_or_else(|| {
                                invalid_state("Package Manifest has no namespace")
                            })?,
                        ),
                        (
                            "name".to_owned(),
                            manifest
                                .get("name")
                                .cloned()
                                .ok_or_else(|| invalid_state("Package Manifest has no name"))?,
                        ),
                        (
                            "version".to_owned(),
                            manifest
                                .get("version")
                                .cloned()
                                .ok_or_else(|| invalid_state("Package Manifest has no version"))?,
                        ),
                        (
                            "kind".to_owned(),
                            manifest
                                .get("kind")
                                .cloned()
                                .ok_or_else(|| invalid_state("Package Manifest has no kind"))?,
                        ),
                        (
                            "capabilities".to_owned(),
                            manifest.get("capabilities").cloned().ok_or_else(|| {
                                invalid_state("Package Manifest has no capabilities")
                            })?,
                        ),
                        (
                            "dependencies".to_owned(),
                            manifest.get("dependencies").cloned().ok_or_else(|| {
                                invalid_state("Package Manifest has no dependencies")
                            })?,
                        ),
                        (
                            "exports".to_owned(),
                            manifest
                                .get("exports")
                                .cloned()
                                .unwrap_or_else(|| Value::Record(BTreeMap::new())),
                        ),
                        (
                            "resources".to_owned(),
                            package_resources_summary(manifest.get("resources"))?,
                        ),
                        (
                            "modules".to_owned(),
                            package_modules_summary(manifest.get("modules"))?,
                        ),
                        (
                            "entry".to_owned(),
                            package_component_summary(manifest.get("entry"))?,
                        ),
                    ]))
                }
                _ => return Err(invalid_state("Package has no Manifest")),
            };
        Ok(Value::Record(BTreeMap::from([
            ("id".to_owned(), Value::Text(package.to_string())),
            (
                "sha256".to_owned(),
                fields
                    .get("sha256")
                    .cloned()
                    .ok_or_else(|| invalid_state("Package has no hash"))?,
            ),
            ("manifest".to_owned(), manifest),
            (
                "valid".to_owned(),
                Value::Bool(self.verify_package_artifact(package)?),
            ),
        ])))
    }

    pub(super) fn verify_package_artifact(&self, package: ObjectId) -> Result<bool, VmError> {
        self.verify_package_artifact_with_dependency_sources(package, None)
    }

    pub(super) fn verify_package_artifact_with_dependency_sources(
        &self,
        package: ObjectId,
        supplied_dependency_sources: Option<&BTreeMap<String, String>>,
    ) -> Result<bool, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, package)?;
        if view.header().type_id != CORE_PACKAGE_TYPE {
            return Err(VmError::TypeError("Object is not a Package"));
        }
        let artifact_version = view.header().version;
        let registry_version = if supplied_dependency_sources.is_none() {
            self.package_registry_object()
                .ok()
                .and_then(|registry| self.manager.read(system, registry).ok())
                .filter(|registry| registry.header().type_id == CORE_PACKAGE_REGISTRY_TYPE)
                .map(|registry| registry.header().version)
        } else {
            None
        };
        if let Some(registry_version) = registry_version {
            if self
                .package_verification_cache
                .lock()
                .ok()
                .and_then(|cache| cache.get(&package).copied())
                == Some((artifact_version, registry_version))
            {
                return Ok(true);
            }
        }
        let value = Value::decode(view.state())?;
        let Value::Record(fields) = value else {
            return Ok(false);
        };
        let manifest = match fields.get("manifest") {
            Some(Value::Record(manifest)) => manifest,
            _ => return Ok(false),
        };
        if manifest.get("format_version") != Some(&Value::Integer(0))
            || manifest.get("praxis_language") != Some(&Value::Integer(0))
        {
            return Ok(false);
        }
        let (Some(Value::Text(namespace)), Some(Value::Text(name)), Some(Value::Text(version))) = (
            manifest.get("namespace"),
            manifest.get("name"),
            manifest.get("version"),
        ) else {
            return Ok(false);
        };
        if validate_coordinate(namespace, "namespace").is_err()
            || validate_coordinate(name, "name").is_err()
            || validate_release(version).is_err()
        {
            return Ok(false);
        }
        let expected = match fields.get("sha256") {
            Some(Value::Text(expected)) => expected,
            _ => return Ok(false),
        };
        if package_sha256(&Value::Record(manifest.clone()).encode()?) != *expected {
            return Ok(false);
        }
        let modules = match manifest.get("modules") {
            Some(Value::Record(modules)) => modules,
            _ => return Ok(false),
        };
        if modules.is_empty() || modules.len() > 256 {
            return Ok(false);
        }
        let mut sources = BTreeMap::new();
        let mut total_source_bytes = 0_usize;
        let mut total_compiled_bytes = 0_usize;
        for (name, value) in modules {
            if !valid_package_module_name(name) {
                return Ok(false);
            }
            let Value::Record(module) = value else {
                return Ok(false);
            };
            let (
                Some(Value::Text(source)),
                Some(Value::Text(source_hash)),
                Some(Value::Bytes(program)),
            ) = (
                module.get("source"),
                module.get("source_sha256"),
                module.get("program"),
            )
            else {
                return Ok(false);
            };
            total_source_bytes = total_source_bytes.saturating_add(source.len());
            total_compiled_bytes = total_compiled_bytes.saturating_add(program.len());
            if package_sha256(source.as_bytes()) != *source_hash
                || Program::decode(program).is_err()
                || source.len() > MAX_PACKAGE_MODULE_BYTES
                || total_source_bytes > MAX_PACKAGE_SOURCE_BYTES
                || total_compiled_bytes > MAX_PACKAGE_COMPILED_BYTES
            {
                return Ok(false);
            }
            sources.insert(name.clone(), source.clone());
        }
        let kind = match manifest.get("kind") {
            Some(Value::Text(kind)) if matches!(kind.as_str(), "library" | "application") => kind,
            _ => return Ok(false),
        };
        let dependencies = match parse_package_dependencies(manifest.get("dependencies")) {
            Ok(dependencies) => dependencies,
            Err(_) => return Ok(false),
        };
        let package_coordinate = format!("{namespace}/{name}/{version}");
        if dependencies
            .iter()
            .any(|dependency| dependency.coordinate == package_coordinate)
        {
            return Ok(false);
        }
        let dependency_sources = if let Some(sources) = supplied_dependency_sources {
            sources.clone()
        } else {
            match self.package_dependency_sources(&package_dependencies_value(&dependencies)) {
                Ok(sources) => sources,
                Err(_) => return Ok(false),
            }
        };
        let mut expanded_source_bytes = 0_usize;
        for module in sources.keys() {
            let loader_sources = sources.clone();
            let loader_dependencies = dependency_sources.clone();
            let wrapper = format!("import \"{module}\"");
            let Ok(program) = compile_with_contextual_loader(&wrapper, |requested, importer| {
                let (source, identity) = package_compile_source(
                    requested,
                    importer,
                    &loader_sources,
                    &loader_dependencies,
                )?;
                expanded_source_bytes = expanded_source_bytes.saturating_add(source.len());
                if expanded_source_bytes > MAX_PACKAGE_EXPANDED_SOURCE_BYTES {
                    return Err("Package compilation expansion exceeds 64 MiB".to_owned());
                }
                Ok((source, identity))
            }) else {
                return Ok(false);
            };
            let Some(Value::Record(module_record)) = modules.get(module) else {
                return Ok(false);
            };
            if module_record.get("program")
                != Some(&Value::Bytes(program.encode().map_err(VmError::from)?))
            {
                return Ok(false);
            }
        }
        if !dependencies.is_empty() {
            let Ok(registry) = self.package_registry_object() else {
                return Ok(false);
            };
            let registry_view = self.manager.read(system, registry)?;
            if registry_view.header().type_id != CORE_PACKAGE_REGISTRY_TYPE {
                return Ok(false);
            }
            for dependency in &dependencies {
                let Some(dependency_artifact) = registry_view
                    .links()
                    .get(&format!("coordinate:{}", dependency.coordinate))
                    .copied()
                else {
                    return Ok(false);
                };
                let dependency_view = self.manager.read(system, dependency_artifact)?;
                if dependency_view.header().type_id != CORE_PACKAGE_TYPE {
                    return Ok(false);
                }
                let Value::Record(dependency_fields) = Value::decode(dependency_view.state())?
                else {
                    return Ok(false);
                };
                if dependency_fields.get("sha256") != Some(&Value::Text(dependency.sha256.clone()))
                    || registry_view
                        .links()
                        .get(&format!("package:{}", dependency.sha256))
                        != Some(&dependency_artifact)
                {
                    return Ok(false);
                }
            }
        }
        if let Some(resource_value) = manifest.get("resources") {
            let Value::Record(resources) = resource_value else {
                return Ok(false);
            };
            if resources.len() > 256 {
                return Ok(false);
            }
            let mut resource_bytes = 0_usize;
            for (name, resource) in resources {
                if !valid_package_module_name(name) {
                    return Ok(false);
                }
                resource_bytes = resource_bytes.saturating_add(resource.encode()?.len());
                if resource_bytes > MAX_PACKAGE_SOURCE_BYTES {
                    return Ok(false);
                }
            }
        }
        let exports = match manifest.get("exports") {
            Some(value) => match normalize_package_exports(Some(value)) {
                Ok(Value::Record(exports)) => exports,
                _ => return Ok(false),
            },
            None => BTreeMap::new(),
        };
        for (_export_name, export_value) in exports {
            let Value::Record(export) = export_value else {
                return Ok(false);
            };
            let (
                Some(Value::Text(module)),
                Some(Value::Text(function)),
                Some(Value::Integer(arity)),
            ) = (
                export.get("module"),
                export.get("function"),
                export.get("arguments"),
            )
            else {
                return Ok(false);
            };
            let Some(Value::Record(module_record)) = modules.get(module) else {
                return Ok(false);
            };
            let Some(Value::Bytes(program)) = module_record.get("program") else {
                return Ok(false);
            };
            let program = Program::decode(program)?;
            let Ok(arity) = usize::try_from(*arity) else {
                return Ok(false);
            };
            if super::find_function(&program, function, arity).is_err() {
                return Ok(false);
            }
        }
        let Some(Value::Array(capabilities)) = manifest.get("capabilities") else {
            return Ok(false);
        };
        if normalize_capabilities(Some(&Value::Array(capabilities.clone()))).is_err() {
            return Ok(false);
        }
        match (kind.as_str(), manifest.get("entry")) {
            ("application", Some(Value::Record(entry))) => {
                let (
                    Some(Value::Text(source)),
                    Some(Value::Text(source_hash)),
                    Some(Value::Bytes(program)),
                ) = (
                    entry.get("source"),
                    entry.get("source_sha256"),
                    entry.get("program"),
                )
                else {
                    return Ok(false);
                };
                total_source_bytes = total_source_bytes.saturating_add(source.len());
                total_compiled_bytes = total_compiled_bytes.saturating_add(program.len());
                if source.len() > MAX_PACKAGE_MODULE_BYTES
                    || total_source_bytes > MAX_PACKAGE_SOURCE_BYTES
                    || total_compiled_bytes > MAX_PACKAGE_COMPILED_BYTES
                    || package_sha256(source.as_bytes()) != *source_hash
                    || Program::decode(program).is_err()
                {
                    return Ok(false);
                }
                let decoded = Program::decode(program)?;
                if super::find_function(&decoded, "main", 0).is_err() {
                    return Ok(false);
                }
                let loader_sources = sources.clone();
                let loader_dependencies = dependency_sources.clone();
                let Ok(compiled) =
                    compile_program_with_contextual_loader(source, |requested, importer| {
                        let (source, identity) = package_compile_source(
                            requested,
                            importer,
                            &loader_sources,
                            &loader_dependencies,
                        )?;
                        expanded_source_bytes = expanded_source_bytes.saturating_add(source.len());
                        if expanded_source_bytes > MAX_PACKAGE_EXPANDED_SOURCE_BYTES {
                            return Err("Package compilation expansion exceeds 64 MiB".to_owned());
                        }
                        Ok((source, identity))
                    })
                else {
                    return Ok(false);
                };
                if compiled.encode().map_err(VmError::from)?.as_slice() != program.as_slice() {
                    return Ok(false);
                }
            }
            ("library", Some(Value::Null)) => {}
            _ => return Ok(false),
        }
        if let Some(registry_version) = registry_version {
            if let Ok(mut cache) = self.package_verification_cache.lock() {
                if cache.len() >= 1024 {
                    cache.clear();
                }
                cache.insert(package, (artifact_version, registry_version));
            }
        }
        Ok(true)
    }
}

fn package_compile_source(
    requested: &str,
    importer: Option<&str>,
    local_modules: &BTreeMap<String, String>,
    dependency_modules: &BTreeMap<String, String>,
) -> Result<(String, String), String> {
    if requested.starts_with("package/") {
        let source = dependency_modules
            .get(requested)
            .cloned()
            .ok_or_else(|| format!("Package dependency does not contain module '{requested}'"))?;
        return Ok((source, requested.to_owned()));
    }
    if let Some(importer) = importer.filter(|importer| importer.starts_with("package/")) {
        let parts = importer.splitn(5, '/').collect::<Vec<_>>();
        if parts.len() != 5 || !valid_package_module_name(requested) {
            return Err("invalid Package dependency import identity".to_owned());
        }
        let identity = format!(
            "package/{}/{}/{}/{}",
            parts[1], parts[2], parts[3], requested
        );
        let source = dependency_modules
            .get(&identity)
            .cloned()
            .ok_or_else(|| format!("Package dependency does not contain module '{requested}'"))?;
        return Ok((source, identity));
    }
    let source = local_modules
        .get(requested)
        .cloned()
        .ok_or_else(|| format!("Package does not contain module '{requested}'"))?;
    Ok((source, requested.to_owned()))
}
