#![allow(clippy::wildcard_imports)]

use super::package_support::*;
use super::*;

struct PackageClosureState<'a> {
    registry_view: &'a ObjectView,
    visiting: BTreeSet<ObjectId>,
    visited: BTreeSet<ObjectId>,
    coordinates: BTreeMap<String, String>,
    packages: Vec<ResolvedPackage>,
}

impl VirtualMachine {
    // Resolve and validate the full immutable closure before staging its links,
    // installations, and default selection in the caller's transaction.
    #[expect(
        clippy::too_many_lines,
        reason = "package installation stages one validated dependency closure"
    )]
    pub(super) fn install_package(
        &self,
        owner: SubjectId,
        registry: ObjectId,
        root_package: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let registry_view = self.manager.read(system, registry)?;
        if registry_view.header().type_id != CORE_PACKAGE_REGISTRY_TYPE {
            return Err(VmError::TypeError("Object is not the Package Registry"));
        }
        let mut closure_state = PackageClosureState {
            registry_view: &registry_view,
            visiting: BTreeSet::new(),
            visited: BTreeSet::new(),
            coordinates: BTreeMap::new(),
            packages: Vec::new(),
        };
        self.resolve_package_closure(root_package, None, 0, &mut closure_state)?;
        let closure = closure_state.packages;
        let mut dependency_sources = BTreeMap::new();
        let mut bundled_module_count = 0_usize;
        for package in &closure {
            for (module_name, module_value) in &package.modules {
                let Value::Record(module) = module_value else {
                    return Err(invalid_state("Package Module is malformed"));
                };
                let (Some(Value::Text(source)), Some(Value::Text(source_hash))) =
                    (module.get("source"), module.get("source_sha256"))
                else {
                    return Err(invalid_state("Package Module source is malformed"));
                };
                bundled_module_count += 1;
                if bundled_module_count > 256 || package_sha256(source.as_bytes()) != *source_hash {
                    return Err(VmError::TypeError(
                        "Package dependency closure exceeds 256 valid Modules",
                    ));
                }
                dependency_sources.insert(
                    package_module_import_name(&package.coordinate, module_name),
                    source.clone(),
                );
            }
        }
        for package in &closure {
            if !self.verify_package_artifact_with_dependency_sources(
                package.package,
                Some(&dependency_sources),
            )? {
                return Err(VmError::TypeError("Package dependency Package is invalid"));
            }
        }

        let (user_index, user_index_version, new_user_index) =
            self.package_user_index(owner, registry, &registry_view, transaction)?;
        let index_view = if new_user_index {
            None
        } else {
            let view = self.manager.read(system, user_index)?;
            if view.header().type_id != CORE_NAMESPACE_TYPE
                || !package_user_index_matches(&Value::decode(view.state())?, owner)
            {
                return Err(invalid_state("Package user index is malformed"));
            }
            Some(view)
        };

        let mut installations = BTreeMap::<String, ObjectId>::new();
        let mut installation_versions = BTreeMap::<ObjectId, Option<ObjectVersion>>::new();
        for package in &closure {
            let existing = index_view
                .as_ref()
                .and_then(|view| {
                    view.links()
                        .get(&package_user_link_key(&package.coordinate))
                })
                .copied();
            let installation = if let Some(existing) = existing {
                let existing_view = self.manager.read(system, existing)?;
                let existing_value = Value::decode(existing_view.state())?;
                if existing_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
                    || !package_installation_matches(
                        &existing_value,
                        owner,
                        package.package,
                        &package.sha256,
                    )
                    || !self.verify_package_installation(owner, existing)?
                {
                    return Err(VmError::Provider(
                        "A different or invalid Package release is installed at this coordinate"
                            .to_owned(),
                    ));
                }
                if package.package == root_package {
                    let Value::Record(mut fields) = existing_value else {
                        return Err(invalid_state("Package Installation is malformed"));
                    };
                    if fields.get("explicit") != Some(&Value::Bool(true)) {
                        fields.insert("explicit".to_owned(), Value::Bool(true));
                        transaction
                            .expect(existing, existing_view.header().version)
                            .update_state(existing, Value::Record(fields).encode()?);
                    }
                }
                installation_versions.insert(existing, Some(existing_view.header().version));
                existing
            } else {
                let installation = ObjectId::new();
                let mut request = CreateObject::new(
                    CORE_PACKAGE_INSTALLATION_TYPE,
                    Value::Record(BTreeMap::from([
                        ("owner".to_owned(), Value::Text(owner.to_string())),
                        (
                            "coordinate".to_owned(),
                            Value::Text(package.coordinate.clone()),
                        ),
                        (
                            "package".to_owned(),
                            Value::Text(package.package.to_string()),
                        ),
                        ("sha256".to_owned(), Value::Text(package.sha256.clone())),
                        ("kind".to_owned(), package.kind.clone()),
                        (
                            "dependencies".to_owned(),
                            package_dependencies_value(&package.dependencies),
                        ),
                        (
                            "explicit".to_owned(),
                            Value::Bool(package.package == root_package),
                        ),
                        (
                            "data_quota_bytes".to_owned(),
                            Value::Integer(i64::try_from(MAX_PACKAGE_DATA_BYTES).map_err(
                                |_| invalid_state("Package Data quota is out of range"),
                            )?),
                        ),
                        ("status".to_owned(), Value::Text("installed".to_owned())),
                    ]))
                    .encode()?,
                )
                .with_id(installation)
                .with_parent(user_index)
                .with_link("package", package.package)
                .with_grant(owner, Capability::Inspect)
                .with_grant(owner, Capability::ViewValue)
                .with_grant(owner, Capability::ReplaceValue)
                .with_grant(owner, Capability::Reparent)
                .with_grant(owner, Capability::Invoke)
                .with_grant(owner, Capability::Retire);
                request.capabilities = [
                    Capability::CreateChild,
                    Capability::Inspect,
                    Capability::Link,
                    Capability::ViewValue,
                    Capability::ReplaceValue,
                    Capability::Reparent,
                    Capability::Invoke,
                    Capability::Retire,
                ]
                .into_iter()
                .collect();
                for dependency in &package.dependencies {
                    let dependency_installation =
                        *installations.get(&dependency.coordinate).ok_or_else(|| {
                            invalid_state("Package dependency install order is broken")
                        })?;
                    request.links.insert(
                        package_dependency_link_key(&dependency.coordinate),
                        dependency_installation,
                    );
                }
                transaction.create(request);

                let module_ids = package
                    .modules
                    .keys()
                    .map(|name| (name.clone(), ObjectId::new()))
                    .collect::<BTreeMap<_, _>>();
                let module_dependencies = module_ids
                    .values()
                    .map(|id| Value::Text(id.to_string()))
                    .collect::<Vec<_>>();
                for (name, module_value) in &package.modules {
                    let Value::Record(module_fields) = module_value else {
                        return Err(invalid_state("Package Manifest module is malformed"));
                    };
                    let module_id = module_ids[name];
                    let state = Value::Record(BTreeMap::from([
                        ("name".to_owned(), Value::Text(name.clone())),
                        (
                            "import_name".to_owned(),
                            Value::Text(package_module_import_name(&package.coordinate, name)),
                        ),
                        (
                            "installation".to_owned(),
                            Value::Text(installation.to_string()),
                        ),
                        (
                            "source_sha256".to_owned(),
                            module_fields.get("source_sha256").cloned().ok_or_else(|| {
                                invalid_state("Package module has no source hash")
                            })?,
                        ),
                        (
                            "package_sha256".to_owned(),
                            Value::Text(package.sha256.clone()),
                        ),
                        (
                            "dependencies".to_owned(),
                            Value::Array(module_dependencies.clone()),
                        ),
                    ]));
                    let mut module_request =
                        CreateObject::new(CORE_PACKAGE_MODULE_TYPE, state.encode()?)
                            .with_id(module_id)
                            .with_parent(installation)
                            .with_link("package", package.package)
                            .with_grant(owner, Capability::Inspect)
                            .with_grant(owner, Capability::ViewValue)
                            .with_grant(owner, Capability::Retire);
                    module_request.capabilities = [
                        Capability::Inspect,
                        Capability::ViewValue,
                        Capability::Link,
                        Capability::Retire,
                    ]
                    .into_iter()
                    .collect();
                    transaction.create(module_request).set_link(
                        installation,
                        package_module_link_key(name),
                        module_id,
                    );
                }
                if !new_user_index {
                    transaction.expect(user_index, user_index_version);
                }
                transaction.set_link(
                    user_index,
                    package_user_link_key(&package.coordinate),
                    installation,
                );
                installation_versions.insert(installation, None);
                installation
            };

            installations.insert(package.coordinate.clone(), installation);
            if installation_versions.get(&installation) == Some(&None) {
                for dependency in &package.dependencies {
                    let dependency_installation =
                        *installations.get(&dependency.coordinate).ok_or_else(|| {
                            invalid_state("Package dependency install order is broken")
                        })?;
                    if let Some(Some(version)) = installation_versions.get(&dependency_installation)
                    {
                        transaction.expect(dependency_installation, *version);
                    }
                    transaction.set_link(
                        dependency_installation,
                        package_required_by_link_key(installation),
                        installation,
                    );
                }
            }
        }
        let root = closure
            .last()
            .ok_or_else(|| invalid_state("Package closure is empty"))?;
        let root_installation = installations
            .get(&root.coordinate)
            .copied()
            .ok_or_else(|| invalid_state("Package root is absent from its dependency closure"))?;
        let package_name = root
            .coordinate
            .rsplit_once('/')
            .map(|(package_name, _)| package_name)
            .ok_or_else(|| invalid_state("Package coordinate is malformed"))?;
        let default_key = package_default_link_key(package_name);
        if index_view
            .as_ref()
            .and_then(|view| view.links().get(&default_key))
            != Some(&root_installation)
        {
            if !new_user_index {
                transaction.expect(user_index, user_index_version);
            }
            transaction.set_link(user_index, default_key, root_installation);
        }
        Ok(root_installation)
    }

    fn resolve_package_closure(
        &self,
        package: ObjectId,
        expected: Option<&PackageDependency>,
        depth: usize,
        state: &mut PackageClosureState<'_>,
    ) -> Result<(), VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let artifact_view = self.manager.read(system, package)?;
        if artifact_view.header().type_id != CORE_PACKAGE_TYPE {
            return Err(VmError::TypeError("Package is invalid"));
        }
        let Value::Record(artifact_fields) = Value::decode(artifact_view.state())? else {
            return Err(invalid_state("Package is malformed"));
        };
        let hash = match artifact_fields.get("sha256") {
            Some(Value::Text(hash)) if valid_sha256(hash) => hash.clone(),
            _ => return Err(invalid_state("Package has no valid SHA-256")),
        };
        if artifact_manifest_hash(&Value::Record(artifact_fields.clone()))? != hash {
            return Err(VmError::TypeError(
                "Package SHA-256 does not match its content",
            ));
        }
        let Some(Value::Record(manifest)) = artifact_fields.get("manifest") else {
            return Err(invalid_state("Package has no Manifest"));
        };
        let coordinate = package_coordinate(manifest)?;
        if state
            .registry_view
            .links()
            .get(&format!("coordinate:{coordinate}"))
            != Some(&package)
            || state.registry_view.links().get(&format!("package:{hash}")) != Some(&package)
        {
            return Err(VmError::Provider(
                "Package is not registered under its immutable coordinate and SHA-256".to_owned(),
            ));
        }
        if expected
            .is_some_and(|expected| expected.coordinate != coordinate || expected.sha256 != hash)
        {
            return Err(VmError::Provider(
                "Package dependency does not match its locked coordinate and SHA-256".to_owned(),
            ));
        }
        if let Some(previous_hash) = state.coordinates.insert(coordinate.clone(), hash.clone()) {
            if previous_hash != hash {
                return Err(VmError::Provider(
                    "Package dependency graph contains conflicting versions of one coordinate"
                        .to_owned(),
                ));
            }
        }
        if state.visited.contains(&package) {
            return Ok(());
        }
        if depth > MAX_PACKAGE_DEPENDENCY_DEPTH {
            return Err(VmError::TypeError("Package dependency depth exceeds 64"));
        }
        if !state.visiting.insert(package) {
            return Err(VmError::Provider(
                "Package dependency cycle detected".to_owned(),
            ));
        }
        if state.visiting.len() + state.visited.len() > MAX_PACKAGE_DEPENDENCIES {
            return Err(VmError::TypeError(
                "Package dependency closure exceeds 256 Packages",
            ));
        }
        let dependencies = parse_package_dependencies(manifest.get("dependencies"))?;
        for dependency in &dependencies {
            let dependency_artifact = state
                .registry_view
                .links()
                .get(&format!("coordinate:{}", dependency.coordinate))
                .copied()
                .ok_or_else(|| VmError::MissingKey(dependency.coordinate.clone()))?;
            self.resolve_package_closure(dependency_artifact, Some(dependency), depth + 1, state)?;
        }
        state.visiting.remove(&package);
        state.visited.insert(package);
        let modules = match manifest.get("modules") {
            Some(Value::Record(modules)) => modules.clone(),
            _ => return Err(invalid_state("Package Manifest modules are malformed")),
        };
        let kind = manifest
            .get("kind")
            .cloned()
            .ok_or_else(|| invalid_state("Package Manifest has no kind"))?;
        state.packages.push(ResolvedPackage {
            package,
            coordinate,
            sha256: hash,
            kind,
            modules,
            dependencies,
        });
        if state.packages.len() > MAX_PACKAGE_DEPENDENCIES {
            return Err(VmError::TypeError(
                "Package dependency closure exceeds 256 Packages",
            ));
        }
        Ok(())
    }

    pub(super) fn installed_packages(&self, owner: SubjectId) -> Result<Value, VmError> {
        let registry = self.package_registry_object()?;
        let registry_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), registry)?;
        let Some(user_index) = registry_view
            .links()
            .get(&package_user_index_key(owner))
            .copied()
        else {
            return Ok(Value::Array(Vec::new()));
        };
        let index_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), user_index)?;
        Ok(Value::Array(
            index_view
                .links()
                .iter()
                .filter(|(key, _)| key.starts_with("package:"))
                .map(|(_, installation)| Value::Text(installation.to_string()))
                .collect(),
        ))
    }

    /// Reports the complete local Package state after an interrupted host boot.
    ///
    /// OMS commits installation changes atomically, so recovery never tries to
    /// guess or replay a half-written install.  It verifies the authoritative
    /// Package and Installation Objects and tells `local` exactly which
    /// records, if any, are inconsistent.
    pub(super) fn package_recovery_report(&self, owner: SubjectId) -> Result<Value, VmError> {
        let registry = self.package_registry_object()?;
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let registry_view = self.manager.read(system, registry)?;
        if registry_view.header().type_id != CORE_PACKAGE_REGISTRY_TYPE {
            return Err(invalid_state("Package Registry has the wrong type"));
        }

        let mut healthy = true;
        let mut artifacts = Vec::new();
        for (key, artifact) in registry_view.links() {
            let Some(coordinate) = key.strip_prefix("coordinate:") else {
                continue;
            };
            let result = self.verify_package_artifact(*artifact);
            let valid = result.as_ref().is_ok_and(|valid| *valid);
            healthy &= valid;
            let mut record = BTreeMap::from([
                ("coordinate".to_owned(), Value::Text(coordinate.to_owned())),
                ("package".to_owned(), Value::Text(artifact.to_string())),
                ("valid".to_owned(), Value::Bool(valid)),
            ]);
            if let Err(error) = result {
                record.insert("error".to_owned(), Value::Text(error.to_string()));
            }
            artifacts.push(Value::Record(record));
        }

        let mut installations = Vec::new();
        if let Some(index) = registry_view
            .links()
            .get(&package_user_index_key(owner))
            .copied()
        {
            let index_view = self.manager.read(system, index)?;
            if index_view.header().type_id != CORE_NAMESPACE_TYPE
                || !package_user_index_matches(&Value::decode(index_view.state())?, owner)
            {
                healthy = false;
                installations.push(Value::Record(BTreeMap::from([
                    ("index".to_owned(), Value::Text(index.to_string())),
                    ("valid".to_owned(), Value::Bool(false)),
                    (
                        "error".to_owned(),
                        Value::Text("Package user index is malformed".to_owned()),
                    ),
                ])));
            } else {
                for (key, installation) in index_view.links() {
                    let Some(coordinate) = key.strip_prefix("package:") else {
                        continue;
                    };
                    let result = self.verify_package_installation(owner, *installation);
                    let valid = result.as_ref().is_ok_and(|valid| *valid);
                    healthy &= valid;
                    let mut record = BTreeMap::from([
                        ("coordinate".to_owned(), Value::Text(coordinate.to_owned())),
                        ("package".to_owned(), Value::Text(installation.to_string())),
                        ("valid".to_owned(), Value::Bool(valid)),
                    ]);
                    if let Err(error) = result {
                        record.insert("error".to_owned(), Value::Text(error.to_string()));
                    }
                    installations.push(Value::Record(record));
                }
            }
        }
        Ok(Value::Record(BTreeMap::from([
            ("healthy".to_owned(), Value::Bool(healthy)),
            ("packages".to_owned(), Value::Array(artifacts)),
            ("packages".to_owned(), Value::Array(installations)),
        ])))
    }

    pub(super) fn search_packages(&self, query: &str) -> Result<Value, VmError> {
        if query.len() > 256 {
            return Err(VmError::TypeError("Package search query is too long"));
        }
        let registry = self.package_registry_object()?;
        let view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), registry)?;
        let mut results = Vec::new();
        for (key, artifact) in view.links() {
            let Some(coordinate) = key.strip_prefix("coordinate:") else {
                continue;
            };
            if !query.is_empty() && !coordinate.contains(query) {
                continue;
            }
            results.push(Value::Record(BTreeMap::from([
                ("coordinate".to_owned(), Value::Text(coordinate.to_owned())),
                ("package".to_owned(), Value::Text(artifact.to_string())),
            ])));
            if results.len() == 100 {
                break;
            }
        }
        Ok(Value::Array(results))
    }

    pub(super) fn find_package_artifact(&self, coordinate: &str) -> Result<ObjectId, VmError> {
        validate_full_coordinate(coordinate)?;
        let registry = self.package_registry_object()?;
        let view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), registry)?;
        let artifact = view
            .links()
            .get(&format!("coordinate:{coordinate}"))
            .copied()
            .ok_or_else(|| VmError::MissingKey(coordinate.to_owned()))?;
        if !self.verify_package_artifact(artifact)? {
            return Err(invalid_state(
                "Package registry contains an invalid Package",
            ));
        }
        Ok(artifact)
    }

    pub(super) fn require_package(
        &self,
        owner: SubjectId,
        coordinate: &str,
    ) -> Result<ObjectId, VmError> {
        let coordinate_parts = coordinate.split('/').collect::<Vec<_>>();
        let default_name = if coordinate_parts.len() == 2 {
            validate_coordinate(coordinate_parts[0], "namespace")?;
            validate_coordinate(coordinate_parts[1], "name")?;
            Some(coordinate)
        } else {
            validate_full_coordinate(coordinate)?;
            None
        };
        let registry = self.package_registry_object()?;
        let registry_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), registry)?;
        let Some(user_index) = registry_view
            .links()
            .get(&package_user_index_key(owner))
            .copied()
        else {
            return Err(VmError::MissingKey(coordinate.to_owned()));
        };
        let index_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), user_index)?;
        let installation = if let Some(package_name) = default_name {
            index_view
                .links()
                .get(&package_default_link_key(package_name))
                .copied()
        } else {
            index_view
                .links()
                .get(&package_user_link_key(coordinate))
                .copied()
        }
        .ok_or_else(|| VmError::MissingKey(coordinate.to_owned()))?;
        let installation_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), installation)?;
        if installation_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
            || !package_installation_owned(&Value::decode(installation_view.state())?, owner)
        {
            return Err(invalid_state("Package installation index is inconsistent"));
        }
        let Value::Record(fields) = Value::decode(installation_view.state())? else {
            return Err(invalid_state("Package Installation is malformed"));
        };
        if let Some(package_name) = default_name {
            if !matches!(fields.get("coordinate"), Some(Value::Text(value)) if value.starts_with(&format!("{package_name}/")))
            {
                return Err(invalid_state(
                    "Package default index points to a different package",
                ));
            }
        } else if fields.get("coordinate") != Some(&Value::Text(coordinate.to_owned())) {
            return Err(invalid_state("Package coordinate index is inconsistent"));
        }
        Ok(installation)
    }

    pub(super) fn package_module(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        name: &str,
    ) -> Result<ObjectId, VmError> {
        if !valid_package_module_name(name) {
            return Err(VmError::TypeError("invalid Package module name"));
        }
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let installation_view = self.manager.read(system, installation)?;
        if installation_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
            || !package_installation_owned(&Value::decode(installation_view.state())?, owner)
        {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        let module = installation_view
            .links()
            .get(&package_module_link_key(name))
            .copied()
            .ok_or_else(|| VmError::MissingKey(name.to_owned()))?;
        let module_view = self.manager.read(system, module)?;
        let Value::Record(fields) = Value::decode(module_view.state())? else {
            return Err(invalid_state("Package Module state is malformed"));
        };
        if module_view.header().type_id != CORE_PACKAGE_MODULE_TYPE
            || fields.get("name") != Some(&Value::Text(name.to_owned()))
            || fields.get("installation") != Some(&Value::Text(installation.to_string()))
        {
            return Err(invalid_state("Package Module index is inconsistent"));
        }
        Ok(module)
    }

    pub(super) fn package_module_source(
        &self,
        owner: SubjectId,
        module: ObjectId,
    ) -> Result<String, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let module_view = self.manager.read(system, module)?;
        if module_view.header().type_id != CORE_PACKAGE_MODULE_TYPE {
            return Err(VmError::TypeError("Object is not a Package Module"));
        }
        let Value::Record(module_fields) = Value::decode(module_view.state())? else {
            return Err(invalid_state("Package Module state is malformed"));
        };
        let installation = match module_fields.get("installation") {
            Some(Value::Text(id)) => id
                .parse::<ObjectId>()
                .map_err(|_| invalid_state("Package Module installation id is malformed"))?,
            _ => return Err(invalid_state("Package Module has no Installation")),
        };
        if module_view.header().parent_id != Some(installation)
            || !package_installation_owned(&self.manager.value(system, installation)?, owner)
        {
            return Err(VmError::TypeError("Package Module belongs to another user"));
        }
        let artifact = module_view
            .links()
            .get("package")
            .copied()
            .ok_or_else(|| invalid_state("Package Module has no Package Link"))?;
        let Value::Record(artifact_fields) = self.manager.value(system, artifact)? else {
            return Err(invalid_state("Package is malformed"));
        };
        let Some(Value::Text(expected_artifact_hash)) = module_fields.get("package_sha256") else {
            return Err(invalid_state("Package Module has no Package hash"));
        };
        if artifact_fields.get("sha256") != Some(&Value::Text(expected_artifact_hash.clone()))
            || artifact_manifest_hash(&Value::Record(artifact_fields.clone()))?
                != *expected_artifact_hash
        {
            return Err(VmError::Provider(
                "Package changed after installation".to_owned(),
            ));
        }
        let Some(Value::Record(manifest)) = artifact_fields.get("manifest") else {
            return Err(invalid_state("Package has no Manifest"));
        };
        let Some(Value::Text(name)) = module_fields.get("name") else {
            return Err(invalid_state("Package Module has no name"));
        };
        let Some(Value::Record(modules)) = manifest.get("modules") else {
            return Err(invalid_state("Package Manifest modules are malformed"));
        };
        let component = modules
            .get(name)
            .ok_or_else(|| invalid_state("Package Module is absent from its Package"))?;
        let Value::Record(component) = component else {
            return Err(invalid_state("Package Module is malformed"));
        };
        let Some(Value::Text(source)) = component.get("source") else {
            return Err(invalid_state("Package Module has no source"));
        };
        let Some(Value::Text(expected_source_hash)) = module_fields.get("source_sha256") else {
            return Err(invalid_state("Package Module has no source hash"));
        };
        if component.get("source_sha256") != Some(&Value::Text(expected_source_hash.clone()))
            || package_sha256(source.as_bytes()) != *expected_source_hash
        {
            return Err(VmError::Provider(
                "Package Module source changed after installation".to_owned(),
            ));
        }
        Ok(source.clone())
    }

    pub(super) fn package_module_source_for_subject(
        &self,
        owner: SubjectId,
        name: &str,
        importer: Option<&str>,
    ) -> Result<(String, String), String> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        if let Some(reference) = name.strip_prefix("package/") {
            let parts = reference.splitn(4, '/').collect::<Vec<_>>();
            if parts.len() != 4 || !valid_package_module_name(parts[3]) {
                return Err("invalid Package module import path".to_owned());
            }
            let coordinate = format!("{}/{}/{}", parts[0], parts[1], parts[2]);
            validate_full_coordinate(&coordinate).map_err(|error| error.to_string())?;
            let installation = self
                .require_package(owner, &coordinate)
                .map_err(|error| error.to_string())?;
            let module = self
                .package_module(owner, installation, parts[3])
                .map_err(|error| error.to_string())?;
            let view = self
                .manager
                .read(system, module)
                .map_err(|error| error.to_string())?;
            let Value::Record(fields) =
                Value::decode(view.state()).map_err(|error| error.to_string())?
            else {
                return Err("Package Module state is malformed".to_owned());
            };
            if fields.get("import_name") != Some(&Value::Text(name.to_owned())) {
                return Err("Package Module import identity does not match".to_owned());
            }
            let source = self
                .package_module_source(owner, module)
                .map_err(|error| error.to_string())?;
            return Ok((source, module.to_string()));
        }

        if let Some(importer) = importer {
            let importer_id = importer
                .parse::<ObjectId>()
                .map_err(|_| "invalid importing Module Object id".to_owned())?;
            let importer_view = self
                .manager
                .read(system, importer_id)
                .map_err(|error| error.to_string())?;
            if importer_view.header().type_id == CORE_PACKAGE_MODULE_TYPE {
                let Value::Record(importer_fields) =
                    Value::decode(importer_view.state()).map_err(|error| error.to_string())?
                else {
                    return Err("importing Package Module is malformed".to_owned());
                };
                let installation = match importer_fields.get("installation") {
                    Some(Value::Text(id)) => id
                        .parse::<ObjectId>()
                        .map_err(|_| "Package Module installation id is malformed".to_owned())?,
                    _ => return Err("Package Module installation id is missing".to_owned()),
                };
                let installation_value = self
                    .manager
                    .value(system, installation)
                    .map_err(|error| error.to_string())?;
                if !package_installation_owned(&installation_value, owner) {
                    return Err("Package Module belongs to another user".to_owned());
                }
                let Some(Value::Array(dependencies)) = importer_fields.get("dependencies") else {
                    return Err("Package Module dependencies are malformed".to_owned());
                };
                for dependency in dependencies {
                    let Value::Text(id) = dependency else {
                        return Err("Package Module dependency id is malformed".to_owned());
                    };
                    let id = id
                        .parse::<ObjectId>()
                        .map_err(|_| "Package Module dependency id is malformed".to_owned())?;
                    let dependency_view = self
                        .manager
                        .read(system, id)
                        .map_err(|error| error.to_string())?;
                    if dependency_view.header().type_id != CORE_PACKAGE_MODULE_TYPE {
                        continue;
                    }
                    let Value::Record(fields) = Value::decode(dependency_view.state())
                        .map_err(|error| error.to_string())?
                    else {
                        continue;
                    };
                    if fields.get("installation") != Some(&Value::Text(installation.to_string()))
                        || fields.get("name") != Some(&Value::Text(name.to_owned()))
                    {
                        continue;
                    }
                    let source = self
                        .package_module_source(owner, id)
                        .map_err(|error| error.to_string())?;
                    return Ok((source, id.to_string()));
                }
                return Err(format!(
                    "Package Module did not include sibling module '{name}'"
                ));
            }
        }

        self.module_source(name, importer)
    }

    pub(super) fn package_installation_info(
        &self,
        owner: SubjectId,
        installation: ObjectId,
    ) -> Result<Value, VmError> {
        let view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), installation)?;
        if view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE {
            return Err(VmError::TypeError("Object is not a Package Installation"));
        }
        let value = Value::decode(view.state())?;
        if !package_installation_owned(&value, owner) {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        Ok(Value::Record(BTreeMap::from([
            ("id".to_owned(), Value::Text(installation.to_string())),
            ("installation".to_owned(), value),
            (
                "capabilities".to_owned(),
                self.package_permissions(owner, installation)?,
            ),
            (
                "valid".to_owned(),
                Value::Bool(self.verify_package_installation(owner, installation)?),
            ),
        ])))
    }

    pub(super) fn verify_package_installation(
        &self,
        owner: SubjectId,
        installation: ObjectId,
    ) -> Result<bool, VmError> {
        self.verify_package_installation_recursive(
            owner,
            installation,
            0,
            &mut BTreeSet::new(),
            &mut BTreeSet::new(),
        )
    }

    // Recursive verification keeps each dependency's coordinate, SHA, reverse
    // links, and active source state under the same validation path.
    #[expect(
        clippy::too_many_lines,
        reason = "recursive installation verification shares one invariant path"
    )]
    fn verify_package_installation_recursive(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        depth: usize,
        visiting: &mut BTreeSet<ObjectId>,
        visited: &mut BTreeSet<ObjectId>,
    ) -> Result<bool, VmError> {
        if visited.contains(&installation) {
            return Ok(true);
        }
        if depth > MAX_PACKAGE_DEPENDENCY_DEPTH {
            return Ok(false);
        }
        if !visiting.insert(installation) {
            return Ok(false);
        }
        if visiting.len() + visited.len() > MAX_PACKAGE_DEPENDENCIES {
            return Ok(false);
        }
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, installation)?;
        if view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE {
            return Err(VmError::TypeError("Object is not a Package Installation"));
        }
        let value = Value::decode(view.state())?;
        if !package_installation_owned(&value, owner) {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        let Value::Record(fields) = value else {
            return Ok(false);
        };
        let (
            Some(Value::Text(artifact_id)),
            Some(Value::Text(expected_hash)),
            Some(Value::Text(coordinate)),
        ) = (
            fields.get("package"),
            fields.get("sha256"),
            fields.get("coordinate"),
        )
        else {
            return Ok(false);
        };
        let Ok(artifact) = artifact_id.parse::<ObjectId>() else {
            return Ok(false);
        };
        let Some(index) = view.header().parent_id else {
            return Ok(false);
        };
        let index_view = self.manager.read(system, index)?;
        if index_view.header().type_id != CORE_NAMESPACE_TYPE
            || !package_user_index_matches(&Value::decode(index_view.state())?, owner)
            || index_view.links().get(&package_user_link_key(coordinate)) != Some(&installation)
            || view.links().get("package") != Some(&artifact)
        {
            return Ok(false);
        }
        let registry = self.package_registry_object()?;
        let registry_view = self.manager.read(system, registry)?;
        if registry_view.header().type_id != CORE_PACKAGE_REGISTRY_TYPE
            || registry_view
                .links()
                .get(&format!("coordinate:{coordinate}"))
                != Some(&artifact)
            || registry_view
                .links()
                .get(&format!("package:{expected_hash}"))
                != Some(&artifact)
        {
            return Ok(false);
        }
        if !self.verify_package_artifact(artifact)? {
            return Ok(false);
        }
        let artifact_value = self.manager.value(system, artifact)?;
        let Value::Record(artifact_fields) = artifact_value else {
            return Ok(false);
        };
        if artifact_fields.get("sha256") != Some(&Value::Text(expected_hash.clone())) {
            return Ok(false);
        }
        let Some(Value::Record(manifest)) = artifact_fields.get("manifest") else {
            return Ok(false);
        };
        if package_coordinate(manifest)? != *coordinate {
            return Ok(false);
        }
        let dependencies = parse_package_dependencies(manifest.get("dependencies"))?;
        if fields.get("dependencies") != Some(&package_dependencies_value(&dependencies))
            || view
                .links()
                .keys()
                .filter(|key| key.starts_with("dependency:"))
                .count()
                != dependencies.len()
        {
            return Ok(false);
        }
        for dependency in &dependencies {
            let Some(dependency_installation) = view
                .links()
                .get(&package_dependency_link_key(&dependency.coordinate))
                .copied()
            else {
                return Ok(false);
            };
            if self.require_package(owner, &dependency.coordinate)? != dependency_installation {
                return Ok(false);
            }
            let dependency_view = self.manager.read(system, dependency_installation)?;
            let dependency_value = Value::decode(dependency_view.state())?;
            let Value::Record(dependency_fields) = dependency_value else {
                return Ok(false);
            };
            let Some(dependency_artifact) = dependency_view.links().get("package").copied() else {
                return Ok(false);
            };
            if dependency_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
                || !package_installation_owned(&Value::Record(dependency_fields.clone()), owner)
                || dependency_fields.get("coordinate")
                    != Some(&Value::Text(dependency.coordinate.clone()))
                || dependency_fields.get("sha256") != Some(&Value::Text(dependency.sha256.clone()))
                || dependency_fields.get("package")
                    != Some(&Value::Text(dependency_artifact.to_string()))
                || dependency_view.links().get("package") != Some(&dependency_artifact)
                || dependency_view
                    .links()
                    .get(&package_required_by_link_key(installation))
                    != Some(&installation)
            {
                return Ok(false);
            }
            let dependency_artifact_value = self.manager.value(system, dependency_artifact)?;
            let Value::Record(dependency_artifact_fields) = dependency_artifact_value else {
                return Ok(false);
            };
            if dependency_artifact_fields.get("sha256")
                != Some(&Value::Text(dependency.sha256.clone()))
                || artifact_manifest_hash(&Value::Record(dependency_artifact_fields))?
                    != dependency.sha256
            {
                return Ok(false);
            }
            if !self.verify_package_installation_recursive(
                owner,
                dependency_installation,
                depth + 1,
                visiting,
                visited,
            )? {
                return Ok(false);
            }
        }
        let Some(Value::Record(manifest_modules)) = manifest.get("modules") else {
            return Ok(false);
        };
        let mut installed_modules = BTreeSet::new();
        for child in view.children() {
            let module_view = self.manager.read(system, *child)?;
            if module_view.header().type_id != CORE_PACKAGE_MODULE_TYPE {
                continue;
            }
            let Value::Record(module_fields) = Value::decode(module_view.state())? else {
                return Ok(false);
            };
            let (
                Some(Value::Text(name)),
                Some(Value::Text(module_installation)),
                Some(Value::Text(module_artifact_hash)),
                Some(Value::Text(source_hash)),
            ) = (
                module_fields.get("name"),
                module_fields.get("installation"),
                module_fields.get("package_sha256"),
                module_fields.get("source_sha256"),
            )
            else {
                return Ok(false);
            };
            let Some(Value::Record(manifest_module)) = manifest_modules.get(name) else {
                return Ok(false);
            };
            if module_view.header().parent_id != Some(installation)
                || module_installation != &installation.to_string()
                || module_artifact_hash != expected_hash
                || manifest_module.get("source_sha256") != Some(&Value::Text(source_hash.clone()))
                || module_view.links().get("package") != Some(&artifact)
                || !installed_modules.insert(name.clone())
            {
                return Ok(false);
            }
        }
        let valid_modules = installed_modules.len() == manifest_modules.len()
            && manifest_modules
                .keys()
                .all(|name| installed_modules.contains(name));
        if valid_modules {
            visiting.remove(&installation);
            visited.insert(installation);
        }
        Ok(valid_modules)
    }

    pub(super) fn package_data(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        package_subject: Option<SubjectId>,
        transaction: &mut Transaction,
    ) -> Result<(ObjectId, bool), VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let installation_view = self.manager.read(system, installation)?;
        if installation_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
            || !package_installation_owned(&Value::decode(installation_view.state())?, owner)
        {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        let data_objects = self.manager.query(
            system,
            &ObjectQuery::new()
                .with_type(CORE_PACKAGE_DATA_TYPE)
                .with_parent(installation),
        )?;
        if data_objects.len() > 1 {
            return Err(invalid_state(
                "Package Installation has multiple private data Objects",
            ));
        }
        if let Some(data) = data_objects.first() {
            return Ok((data.id, false));
        }

        let data = ObjectId::new();
        let mut request = CreateObject::new(CORE_PACKAGE_DATA_TYPE, Value::Null.encode()?)
            .with_id(data)
            .with_parent(installation)
            .with_grant(owner, Capability::Inspect)
            .with_grant(owner, Capability::ViewValue)
            .with_grant(owner, Capability::ReplaceValue)
            .with_grant(owner, Capability::Retire);
        if let Some(subject) = package_subject.filter(|subject| *subject != owner) {
            request = request
                .with_grant(subject, Capability::Inspect)
                .with_grant(subject, Capability::ViewValue)
                .with_grant(subject, Capability::ReplaceValue);
        }
        request.capabilities = [
            Capability::Inspect,
            Capability::ViewValue,
            Capability::ReplaceValue,
            Capability::ManagePolicy,
            Capability::Retire,
        ]
        .into_iter()
        .collect();
        transaction
            .expect(installation, installation_view.header().version)
            .create(request);
        Ok((data, true))
    }

    pub(super) fn package_data_info(
        &self,
        owner: SubjectId,
        installation: ObjectId,
    ) -> Result<Value, VmError> {
        self.ensure_package_installation_owner(owner, installation)?;
        let data = self.package_data_object(installation)?;
        let (id, used) = if let Some(data) = data {
            let value = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), data)?;
            (
                Value::Text(data.to_string()),
                Value::Integer(
                    i64::try_from(value.encode()?.len())
                        .map_err(|_| invalid_state("Package Data size is out of range"))?,
                ),
            )
        } else {
            (Value::Null, Value::Integer(0))
        };
        let quota = i64::try_from(self.package_data_quota_bytes(installation)?)
            .map_err(|_| invalid_state("Package Data quota is out of range"))?;
        Ok(Value::Record(BTreeMap::from([
            ("data".to_owned(), id),
            ("used_bytes".to_owned(), used.clone()),
            ("quota_bytes".to_owned(), Value::Integer(quota)),
            (
                "available_bytes".to_owned(),
                Value::Integer(quota.saturating_sub(match used {
                    Value::Integer(value) => value,
                    _ => 0,
                })),
            ),
        ])))
    }

    pub(super) fn package_data_quota(
        &self,
        owner: SubjectId,
        installation: ObjectId,
    ) -> Result<Value, VmError> {
        self.ensure_package_installation_owner(owner, installation)?;
        Ok(Value::Integer(
            i64::try_from(self.package_data_quota_bytes(installation)?)
                .map_err(|_| invalid_state("Package Data quota is out of range"))?,
        ))
    }

    pub(super) fn set_package_data_quota(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        quota: i64,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        if owner != SYSTEM_SUBJECT {
            return Err(VmError::TypeError(
                "only local can change a Package Data quota",
            ));
        }
        let quota = usize::try_from(quota)
            .map_err(|_| VmError::TypeError("Package Data quota must not be negative"))?;
        if quota > MAX_PACKAGE_DATA_BYTES {
            return Err(VmError::TypeError(
                "Package Data quota cannot exceed the 8 MiB system limit",
            ));
        }
        let view = self.ensure_package_installation_owner(owner, installation)?;
        if let Some(data) = self.package_data_object(installation)? {
            let size = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), data)?
                .encode()?
                .len();
            if size > quota {
                return Err(VmError::TypeError(
                    "Package Data quota cannot be below its current usage",
                ));
            }
        }
        let Value::Record(mut fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Package Installation is malformed"));
        };
        fields.insert(
            "data_quota_bytes".to_owned(),
            Value::Integer(
                i64::try_from(quota)
                    .map_err(|_| invalid_state("Package Data quota is out of range"))?,
            ),
        );
        transaction
            .expect(installation, view.header().version)
            .update_state(installation, Value::Record(fields).encode()?);
        Ok(())
    }

    pub(super) fn reset_package_data_quota(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        self.set_package_data_quota(
            owner,
            installation,
            i64::try_from(MAX_PACKAGE_DATA_BYTES)
                .map_err(|_| invalid_state("Package Data quota is out of range"))?,
            transaction,
        )
    }

    pub(super) fn package_data_export(
        &self,
        owner: SubjectId,
        installation: ObjectId,
    ) -> Result<Value, VmError> {
        self.ensure_package_installation_owner(owner, installation)?;
        self.package_data_object(installation)?.map_or_else(
            || Ok(Value::Null),
            |data| {
                self.manager
                    .value(AccessContext::new(SYSTEM_SUBJECT), data)
                    .map_err(Into::into)
            },
        )
    }

    pub(super) fn package_data_import(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        snapshot: &Value,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let installation_view = self.ensure_package_installation_owner(owner, installation)?;
        let encoded = snapshot.encode()?;
        if encoded.len() > self.package_data_quota_bytes(installation)? {
            return Err(VmError::TypeError(
                "Package Data exceeds this Package's quota",
            ));
        }
        if let Some(data) = self.package_data_object(installation)? {
            let view = self
                .manager
                .read(AccessContext::new(SYSTEM_SUBJECT), data)?;
            transaction
                .expect(data, view.header().version)
                .update_state(data, encoded);
            return Ok(());
        }

        let data = ObjectId::new();
        let mut request = CreateObject::new(CORE_PACKAGE_DATA_TYPE, encoded)
            .with_id(data)
            .with_parent(installation)
            .with_grant(owner, Capability::Inspect)
            .with_grant(owner, Capability::ViewValue)
            .with_grant(owner, Capability::ReplaceValue)
            .with_grant(owner, Capability::Retire);
        request.capabilities = [
            Capability::Inspect,
            Capability::ViewValue,
            Capability::ReplaceValue,
            Capability::Retire,
        ]
        .into_iter()
        .collect();
        transaction
            .expect(installation, installation_view.header().version)
            .create(request);
        Ok(())
    }

    fn ensure_package_installation_owner(
        &self,
        owner: SubjectId,
        installation: ObjectId,
    ) -> Result<ObjectView, VmError> {
        let view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), installation)?;
        if view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
            || !package_installation_owned(&Value::decode(view.state())?, owner)
        {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        Ok(view)
    }

    fn package_data_object(&self, installation: ObjectId) -> Result<Option<ObjectId>, VmError> {
        let data_objects = self.manager.query(
            AccessContext::new(SYSTEM_SUBJECT),
            &ObjectQuery::new()
                .with_type(CORE_PACKAGE_DATA_TYPE)
                .with_parent(installation),
        )?;
        if data_objects.len() > 1 {
            return Err(invalid_state(
                "Package Installation has multiple private data Objects",
            ));
        }
        Ok(data_objects.first().map(|data| data.id))
    }

    pub(super) fn package_data_quota_for_object(&self, data: ObjectId) -> Result<usize, VmError> {
        let view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), data)?;
        if view.header().type_id != CORE_PACKAGE_DATA_TYPE {
            return Err(VmError::TypeError("Object is not Package Data"));
        }
        let installation = view
            .header()
            .parent_id
            .ok_or_else(|| invalid_state("Package Data has no Installation parent"))?;
        self.package_data_quota_bytes(installation)
    }

    fn package_data_quota_bytes(&self, installation: ObjectId) -> Result<usize, VmError> {
        let value = self
            .manager
            .value(AccessContext::new(SYSTEM_SUBJECT), installation)?;
        let Value::Record(fields) = value else {
            return Err(invalid_state("Package Installation is malformed"));
        };
        match fields.get("data_quota_bytes") {
            // Old development-state installs did not record this field.  Keep
            // their existing 8 MiB behaviour rather than silently changing
            // persisted data while format 0 is still in development.
            None => Ok(MAX_PACKAGE_DATA_BYTES),
            Some(Value::Integer(quota)) => {
                let quota = usize::try_from(*quota)
                    .map_err(|_| invalid_state("Package Data quota is negative"))?;
                if quota > MAX_PACKAGE_DATA_BYTES {
                    return Err(invalid_state("Package Data quota exceeds the system limit"));
                }
                Ok(quota)
            }
            _ => Err(invalid_state("Package Data quota is malformed")),
        }
    }

    pub(super) fn package_resource(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        name: &str,
    ) -> Result<Value, VmError> {
        if !valid_package_module_name(name) {
            return Err(VmError::TypeError("invalid Package resource name"));
        }
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let installation_view = self.manager.read(system, installation)?;
        if installation_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
            || !package_installation_owned(&Value::decode(installation_view.state())?, owner)
        {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        let artifact = installation_view
            .links()
            .get("package")
            .copied()
            .ok_or_else(|| invalid_state("Package Installation has no Package"))?;
        if !self.verify_package_artifact(artifact)? {
            return Err(VmError::Provider("Package verification failed".to_owned()));
        }
        let artifact = self.manager.value(system, artifact)?;
        let Value::Record(artifact_fields) = artifact else {
            return Err(invalid_state("Package is malformed"));
        };
        let Some(Value::Record(manifest)) = artifact_fields.get("manifest") else {
            return Err(invalid_state("Package Manifest is malformed"));
        };
        let Some(Value::Record(resources)) = manifest.get("resources") else {
            return Err(VmError::MissingKey(name.to_owned()));
        };
        resources
            .get(name)
            .cloned()
            .ok_or_else(|| VmError::MissingKey(name.to_owned()))
    }

    pub(super) fn package_permissions(
        &self,
        owner: SubjectId,
        installation: ObjectId,
    ) -> Result<Value, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let installation_value = self.manager.value(system, installation)?;
        if !package_installation_owned(&installation_value, owner) {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        let Value::Record(installation_fields) = installation_value else {
            return Err(invalid_state("Package Installation state is malformed"));
        };
        let artifact = match installation_fields.get("package") {
            Some(Value::Text(artifact)) => artifact
                .parse::<ObjectId>()
                .map_err(|_| invalid_state("Package Installation Package ID is malformed"))?,
            _ => return Err(invalid_state("Package Installation has no Package")),
        };
        let Value::Record(artifact_fields) = self.manager.value(system, artifact)? else {
            return Err(invalid_state("Package is malformed"));
        };
        let Some(Value::Record(manifest)) = artifact_fields.get("manifest") else {
            return Err(invalid_state("Package has no Manifest"));
        };
        let capabilities = normalize_capabilities(manifest.get("capabilities"))?;
        Ok(Value::Array(
            capabilities
                .into_iter()
                .map(|capability| {
                    let grantable = self.package_capability_targets(&capability).is_ok();
                    Value::Record(BTreeMap::from([
                        ("capability".to_owned(), Value::Text(capability)),
                        ("grantable".to_owned(), Value::Bool(grantable)),
                    ]))
                })
                .collect(),
        ))
    }

    // Application setup binds Process, Subject, resources, capabilities, and
    // package version before publishing the runnable instance.
    #[expect(
        clippy::too_many_lines,
        reason = "application startup stages interdependent security state"
    )]
    pub(super) fn run_package_application(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        arguments: &Value,
        requested_capabilities: &[String],
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let installation_view = self.manager.read(system, installation)?;
        let installation_value = Value::decode(installation_view.state())?;
        if installation_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
            || !package_installation_owned(&installation_value, owner)
        {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        if !self.verify_package_installation(owner, installation)? {
            return Err(VmError::TypeError("Package Installation is invalid"));
        }
        let Value::Record(installation_fields) = installation_value else {
            return Err(invalid_state("Package Installation state is malformed"));
        };
        let artifact = match installation_fields.get("package") {
            Some(Value::Text(id)) => id
                .parse::<ObjectId>()
                .map_err(|_| invalid_state("Package Installation Package ID is malformed"))?,
            _ => return Err(invalid_state("Package Installation has no Package")),
        };
        let artifact_value = self.manager.value(system, artifact)?;
        let Value::Record(artifact_fields) = artifact_value else {
            return Err(invalid_state("Package is malformed"));
        };
        let Some(Value::Record(manifest)) = artifact_fields.get("manifest") else {
            return Err(invalid_state("Package has no Manifest"));
        };
        if manifest.get("kind") != Some(&Value::Text("application".to_owned())) {
            return Err(VmError::TypeError("only an Application Package can be run"));
        }
        let requested = manifest
            .get("capabilities")
            .and_then(|value| match value {
                Value::Array(values) => Some(values),
                _ => None,
            })
            .ok_or_else(|| invalid_state("Package Manifest capabilities are malformed"))?
            .iter()
            .map(|value| match value {
                Value::Text(value) => Ok(value.clone()),
                _ => Err(invalid_state("Package Manifest capability is malformed")),
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let mut grants = BTreeSet::new();
        for capability in requested_capabilities {
            if !requested.contains(capability) || !grants.insert(capability.clone()) {
                return Err(VmError::TypeError(
                    "requested Package capability is undeclared or duplicated",
                ));
            }
        }
        let Some(Value::Record(entry)) = manifest.get("entry") else {
            return Err(invalid_state("Application Manifest entry is malformed"));
        };
        let resources = match manifest.get("resources") {
            Some(Value::Record(resources)) => Value::Record(resources.clone()),
            None => Value::Record(BTreeMap::new()),
            _ => {
                return Err(invalid_state(
                    "Application Manifest resources are malformed",
                ));
            }
        };
        let Some(Value::Bytes(program_bytes)) = entry.get("program") else {
            return Err(invalid_state("Application Manifest has no Program"));
        };
        let program = Program::decode(program_bytes)?;
        let (entrypoint, parameters) = super::find_function(&program, "main", 0)?;
        if !parameters.is_empty() {
            return Err(VmError::TypeError(
                "Application main() must take no arguments",
            ));
        }
        let halt = program
            .tokens
            .iter()
            .position(|token| matches!(token, Token::Halt))
            .and_then(|position| u32::try_from(position).ok())
            .ok_or(VmError::TypeError("Application Program has no halt token"))?;

        let package_subject = SubjectId::new();
        let instance = ObjectId::new();
        let program_id = ObjectId::new();
        let arguments_id = ObjectId::new();
        let resources_id = ObjectId::new();
        let subject_object = ObjectId::new();
        let process = ObjectId::new();
        let (package_data, package_data_created) =
            self.package_data(owner, installation, Some(package_subject), transaction)?;
        if !package_data_created {
            let data_version = self.manager.inspect(system, package_data)?.version;
            transaction
                .expect(package_data, data_version)
                .grant(package_data, package_subject, Capability::Inspect)
                .grant(package_data, package_subject, Capability::ViewValue)
                .grant(package_data, package_subject, Capability::ReplaceValue);
        }
        let mut module_request = CreateObject::new(
            CORE_PACKAGE_INSTANCE_TYPE,
            Value::Record(BTreeMap::from([
                ("owner".to_owned(), Value::Text(owner.to_string())),
                (
                    "installation".to_owned(),
                    Value::Text(installation.to_string()),
                ),
                ("package".to_owned(), Value::Text(artifact.to_string())),
                (
                    "sha256".to_owned(),
                    installation_fields
                        .get("sha256")
                        .cloned()
                        .ok_or_else(|| invalid_state("Package Installation has no SHA-256"))?,
                ),
                (
                    "subject".to_owned(),
                    Value::Text(package_subject.to_string()),
                ),
                ("program".to_owned(), Value::Text(program_id.to_string())),
                ("process".to_owned(), Value::Text(process.to_string())),
                ("status".to_owned(), Value::Text("running".to_owned())),
            ]))
            .encode()?,
        )
        .with_id(instance)
        .with_parent(installation)
        .with_link("installation", installation)
        .with_link("package", artifact)
        .with_grant(owner, Capability::Inspect)
        .with_grant(owner, Capability::ViewValue)
        .with_grant(owner, Capability::Invoke)
        .with_grant(owner, Capability::Reparent);
        module_request.capabilities = [
            Capability::CreateChild,
            Capability::Inspect,
            Capability::Link,
            Capability::ViewValue,
            Capability::Invoke,
            Capability::Reparent,
            Capability::Retire,
        ]
        .into_iter()
        .collect();
        transaction
            .expect(installation, installation_view.header().version)
            .create(module_request);

        let mut program_request = CreateObject::new(CORE_PROGRAM_TYPE, program.encode()?)
            .with_id(program_id)
            .with_parent(instance)
            .with_grant(package_subject, Capability::Inspect)
            .with_grant(package_subject, Capability::ViewValue);
        program_request.capabilities = [Capability::Inspect, Capability::ViewValue]
            .into_iter()
            .collect();
        let arguments_request = CreateObject::new(CORE_VALUE_TYPE, arguments.encode()?)
            .with_id(arguments_id)
            .with_parent(instance)
            .with_grant(package_subject, Capability::Inspect)
            .with_grant(package_subject, Capability::ViewValue);
        let resources_request = CreateObject::new(CORE_VALUE_TYPE, resources.encode()?)
            .with_id(resources_id)
            .with_parent(instance)
            .with_grant(package_subject, Capability::Inspect)
            .with_grant(package_subject, Capability::ViewValue);
        let mut subject_request = CreateObject::new(
            CORE_PACKAGE_SUBJECT_TYPE,
            Value::Record(BTreeMap::from([
                ("owner".to_owned(), Value::Text(owner.to_string())),
                (
                    "subject".to_owned(),
                    Value::Text(package_subject.to_string()),
                ),
                (
                    "installation".to_owned(),
                    Value::Text(installation.to_string()),
                ),
                (
                    "capabilities".to_owned(),
                    Value::Array(grants.iter().cloned().map(Value::Text).collect()),
                ),
            ]))
            .encode()?,
        )
        .with_id(subject_object)
        .with_parent(instance)
        .with_grant(owner, Capability::Inspect)
        .with_grant(owner, Capability::ViewValue)
        .with_grant(package_subject, Capability::Inspect)
        .with_grant(package_subject, Capability::ViewValue);
        subject_request.capabilities = [Capability::Inspect, Capability::ViewValue]
            .into_iter()
            .collect();
        transaction
            .create(program_request)
            .create(arguments_request)
            .create(resources_request)
            .create(subject_request);

        let mut granted_services = BTreeSet::new();
        for capability in &grants {
            for target in self.package_capability_targets(capability)? {
                if !granted_services.insert(target) {
                    continue;
                }
                let version = self.manager.inspect(system, target)?.version;
                transaction
                    .expect(target, version)
                    .grant(target, package_subject, Capability::Inspect)
                    .grant(target, package_subject, Capability::ViewValue)
                    .grant(target, package_subject, Capability::Invoke);
            }
        }

        let mut state = ProcessState {
            program: program_id,
            subject: package_subject,
            token_position: entrypoint,
            stack: Vec::new(),
            variables: BTreeMap::from([("arguments".to_owned(), arguments_id)]),
            status: ProcessStatus::Ready,
            wait_reason: WaitReason::None,
            lease_owner: None,
            lease_generation: 0,
            lease_deadline_unix_ms: None,
            result: None,
            error: None,
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
        let mut process_request = CreateObject::new(PROCESS_TYPE, encode_process_state(&state)?)
            .with_id(process)
            .with_parent(instance)
            .with_link("program", program_id)
            .with_link("package_instance", instance)
            .with_link("package_subject", subject_object)
            .with_link("package_installation", installation)
            .with_link("package_data", package_data)
            .with_link("package_resources", resources_id)
            .with_link("process", process)
            .with_grant(owner, Capability::Inspect)
            .with_grant(owner, Capability::ViewValue)
            .with_grant(owner, Capability::Invoke)
            .with_grant(owner, Capability::Reparent)
            .with_grant(owner, Capability::Retire)
            .with_grant(package_subject, Capability::Inspect)
            .with_grant(package_subject, Capability::ViewValue)
            .with_grant(package_subject, Capability::ReplaceValue)
            .with_grant(package_subject, Capability::CreateChild)
            .with_grant(package_subject, Capability::Invoke)
            .with_grant(package_subject, Capability::Link)
            .with_grant(package_subject, Capability::Reparent)
            .with_grant(package_subject, Capability::Retire);
        if let Some(console) = self.console_provider {
            process_request = process_request.with_link("console", console);
        }
        for (name, service) in &self.kernel_services {
            process_request = process_request.with_link(name.clone(), *service);
        }
        process_request.capabilities = [
            Capability::Inspect,
            Capability::ViewValue,
            Capability::ReplaceValue,
            Capability::CreateChild,
            Capability::Invoke,
            Capability::Link,
            Capability::Reparent,
            Capability::Retire,
        ]
        .into_iter()
        .collect();
        state.program = program_id;
        transaction
            .create(process_request)
            .set_link(instance, "program", program_id)
            .set_link(instance, "arguments", arguments_id)
            .set_link(instance, "resources", resources_id)
            .set_link(instance, "subject", subject_object)
            .set_link(instance, "process", process)
            .set_link(instance, "data", package_data);
        Ok(process)
    }

    pub(super) fn upgrade_package(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        package: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        let current = self
            .manager
            .value(AccessContext::new(SYSTEM_SUBJECT), installation)?;
        if !package_installation_owned(&current, owner)
            || !self.verify_package_installation(owner, installation)?
        {
            return Err(VmError::TypeError(
                "Package Installation is invalid or belongs to another user",
            ));
        }
        let Value::Record(current_fields) = current else {
            return Err(invalid_state("Package Installation is malformed"));
        };
        let Some(Value::Text(current_coordinate)) = current_fields.get("coordinate") else {
            return Err(invalid_state("Package Installation has no coordinate"));
        };
        let target_value = self
            .manager
            .value(AccessContext::new(SYSTEM_SUBJECT), package)?;
        let Value::Record(target_fields) = target_value else {
            return Err(VmError::TypeError("Upgrade target is not a Package"));
        };
        if !self.verify_package_artifact(package)? {
            return Err(VmError::TypeError("Upgrade target Package is invalid"));
        }
        let Some(Value::Record(target_manifest)) = target_fields.get("manifest") else {
            return Err(invalid_state("Upgrade target Manifest is malformed"));
        };
        let target_coordinate = package_coordinate(target_manifest)?;
        let current_name = current_coordinate
            .rsplit_once('/')
            .map(|(name, _)| name)
            .ok_or_else(|| invalid_state("Package Installation coordinate is malformed"))?;
        let target_name = target_coordinate
            .rsplit_once('/')
            .map(|(name, _)| name)
            .ok_or_else(|| invalid_state("Upgrade target coordinate is malformed"))?;
        if current_name != target_name || *current_coordinate == target_coordinate {
            return Err(VmError::TypeError(
                "upgrade target must be a different version of the same Package",
            ));
        }
        self.install_package(owner, self.package_registry_object()?, package, transaction)
    }

    pub(super) fn rollback_package(
        &self,
        owner: SubjectId,
        current: ObjectId,
        target: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        let current_value = self
            .manager
            .value(AccessContext::new(SYSTEM_SUBJECT), current)?;
        if !package_installation_owned(&current_value, owner)
            || !self.verify_package_installation(owner, current)?
        {
            return Err(VmError::TypeError(
                "Package Installation is invalid or belongs to another user",
            ));
        }
        let target = if self
            .manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), target)
            .is_ok_and(|header| header.type_id == CORE_PACKAGE_INSTALLATION_TYPE)
        {
            target
        } else {
            return Err(VmError::TypeError(
                "rollback target is not an active Package Installation",
            ));
        };
        let target_value = self
            .manager
            .value(AccessContext::new(SYSTEM_SUBJECT), target)?;
        if !package_installation_owned(&target_value, owner)
            || !self.verify_package_installation(owner, target)?
        {
            return Err(VmError::TypeError(
                "rollback target is invalid or belongs to another user",
            ));
        }
        let (Value::Record(current_fields), Value::Record(target_fields)) =
            (current_value, target_value)
        else {
            return Err(invalid_state("Package Installation is malformed"));
        };
        let (Some(Value::Text(current_coordinate)), Some(Value::Text(target_coordinate))) = (
            current_fields.get("coordinate"),
            target_fields.get("coordinate"),
        ) else {
            return Err(invalid_state(
                "Package Installation coordinate is malformed",
            ));
        };
        let package_name = current_coordinate
            .rsplit_once('/')
            .map(|(name, _)| name)
            .ok_or_else(|| invalid_state("Package Installation coordinate is malformed"))?;
        if target_coordinate.rsplit_once('/').map(|(name, _)| name) != Some(package_name) {
            return Err(VmError::TypeError(
                "rollback target belongs to a different Package",
            ));
        }
        let index = self
            .manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), current)?
            .parent_id
            .ok_or_else(|| invalid_state("Package Installation has no user index"))?;
        let index_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), index)?;
        if index_view.header().type_id != CORE_NAMESPACE_TYPE
            || index_view
                .links()
                .get(&package_user_link_key(target_coordinate))
                != Some(&target)
        {
            return Err(invalid_state(
                "rollback target is absent from the user's Package index",
            ));
        }
        let key = package_default_link_key(package_name);
        if index_view.links().get(&key) != Some(&target) {
            transaction
                .expect(index, index_view.header().version)
                .set_link(index, key, target);
        }
        Ok(target)
    }

    #[expect(
        clippy::too_many_lines,
        reason = "restore validates and rebuilds one retired installation"
    )]
    pub(super) fn restore_package(
        &self,
        owner: SubjectId,
        registry: ObjectId,
        retired_installation: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let retired_view = self.manager.read_retained(system, retired_installation)?;
        if retired_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE {
            return Err(VmError::TypeError(
                "Object is not a retired Package Installation",
            ));
        }
        let Value::Record(fields) = Value::decode(retired_view.state())? else {
            return Err(VmError::TypeError(
                "retired Package Installation payload is no longer available",
            ));
        };
        if fields.get("owner") != Some(&Value::Text(owner.to_string()))
            || fields.get("status") != Some(&Value::Text("retired".to_owned()))
        {
            return Err(VmError::TypeError(
                "retired Package Installation belongs to another user or has no restore data",
            ));
        }
        let (
            Some(Value::Text(coordinate)),
            Some(Value::Text(artifact_id)),
            Some(Value::Text(expected_hash)),
            Some(Value::Bool(data_present)),
            Some(data_snapshot),
        ) = (
            fields.get("coordinate"),
            fields.get("package"),
            fields.get("sha256"),
            fields.get("data_present"),
            fields.get("data_snapshot"),
        )
        else {
            return Err(invalid_state(
                "retired Package restore metadata is incomplete",
            ));
        };
        validate_full_coordinate(coordinate)?;
        let artifact = artifact_id
            .parse::<ObjectId>()
            .map_err(|_| invalid_state("retired Package ID is malformed"))?;
        let artifact_view = self.manager.read(system, artifact)?;
        if artifact_view.header().type_id != CORE_PACKAGE_TYPE
            || !self.verify_package_artifact(artifact)?
        {
            return Err(VmError::TypeError(
                "retired Package is unavailable or invalid",
            ));
        }
        let Value::Record(artifact_fields) = Value::decode(artifact_view.state())? else {
            return Err(invalid_state("retired Package is malformed"));
        };
        if artifact_fields.get("sha256") != Some(&Value::Text(expected_hash.clone()))
            || artifact_manifest_hash(&Value::Record(artifact_fields.clone()))? != *expected_hash
        {
            return Err(VmError::TypeError("retired Package SHA-256 changed"));
        }
        let registry_view = self.manager.read(system, registry)?;
        if registry_view.header().type_id != CORE_PACKAGE_REGISTRY_TYPE
            || registry_view
                .links()
                .get(&format!("coordinate:{coordinate}"))
                != Some(&artifact)
        {
            return Err(VmError::TypeError(
                "retired Package is absent from the local registry",
            ));
        }
        let user_index = registry_view
            .links()
            .get(&package_user_index_key(owner))
            .copied()
            .ok_or_else(|| VmError::MissingKey(coordinate.clone()))?;
        let index_view = self.manager.read(system, user_index)?;
        if index_view.header().type_id != CORE_NAMESPACE_TYPE
            || !package_user_index_matches(&Value::decode(index_view.state())?, owner)
        {
            return Err(invalid_state("Package user index is malformed"));
        }
        let restore_key = package_restore_link_key(retired_installation);
        if let Some(restored) = index_view.links().get(&restore_key).copied() {
            let restored_value = self.manager.value(system, restored)?;
            if package_installation_matches(&restored_value, owner, artifact, expected_hash)
                && matches!(
                    &restored_value,
                    Value::Record(restored_fields)
                        if restored_fields.get("coordinate") == Some(&Value::Text(coordinate.clone()))
                )
                && self.verify_package_installation(owner, restored)?
            {
                return Ok(restored);
            }
            return Err(invalid_state(
                "Package restore index points to an invalid Installation",
            ));
        }
        if index_view
            .links()
            .contains_key(&package_user_link_key(coordinate))
        {
            return Err(VmError::Provider(
                "Package coordinate is already installed; restore would overwrite its data"
                    .to_owned(),
            ));
        }
        let installation = self.install_package(owner, registry, artifact, transaction)?;
        if *data_present {
            let encoded_data = data_snapshot.encode()?;
            if encoded_data.len() > MAX_PACKAGE_DATA_BYTES {
                return Err(VmError::TypeError(
                    "retired Package Data exceeds the 8 MiB restore limit",
                ));
            }
            let mut data_request = CreateObject::new(CORE_PACKAGE_DATA_TYPE, encoded_data)
                .with_parent(installation)
                .with_grant(owner, Capability::Inspect)
                .with_grant(owner, Capability::ViewValue)
                .with_grant(owner, Capability::ReplaceValue)
                .with_grant(owner, Capability::Retire);
            data_request.capabilities = [
                Capability::Inspect,
                Capability::ViewValue,
                Capability::ReplaceValue,
                Capability::Retire,
            ]
            .into_iter()
            .collect();
            transaction.create(data_request);
        }
        transaction
            .expect(user_index, index_view.header().version)
            .set_link(user_index, restore_key, installation);
        Ok(installation)
    }

    // Export invocation binds a fresh Process to a verified immutable package
    // version and checks caller capabilities before it becomes runnable.
    #[expect(
        clippy::too_many_lines,
        reason = "export startup stages interdependent security state"
    )]
    pub(super) fn run_package_export(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        export_name: &str,
        arguments: &[Value],
        transaction: &mut Transaction,
    ) -> Result<ObjectId, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let installation_view = self.manager.read(system, installation)?;
        let installation_value = Value::decode(installation_view.state())?;
        if installation_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
            || !package_installation_owned(&installation_value, owner)
        {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        if !self.verify_package_installation(owner, installation)? {
            return Err(VmError::TypeError("Package Installation is invalid"));
        }
        let Value::Record(installation_fields) = installation_value else {
            return Err(invalid_state("Package Installation state is malformed"));
        };
        let artifact = match installation_fields.get("package") {
            Some(Value::Text(id)) => id
                .parse::<ObjectId>()
                .map_err(|_| invalid_state("Package Installation Package ID is malformed"))?,
            _ => return Err(invalid_state("Package Installation has no Package")),
        };
        let artifact_value = self.manager.value(system, artifact)?;
        let Value::Record(artifact_fields) = artifact_value else {
            return Err(invalid_state("Package is malformed"));
        };
        let Some(Value::Record(manifest)) = artifact_fields.get("manifest") else {
            return Err(invalid_state("Package Manifest is malformed"));
        };
        let Some(Value::Record(exports)) = manifest.get("exports") else {
            return Err(VmError::MissingKey(export_name.to_owned()));
        };
        let Some(Value::Record(export)) = exports.get(export_name) else {
            return Err(VmError::MissingKey(export_name.to_owned()));
        };
        let (
            Some(Value::Text(module_name)),
            Some(Value::Text(function_name)),
            Some(Value::Integer(arity)),
        ) = (
            export.get("module"),
            export.get("function"),
            export.get("arguments"),
        )
        else {
            return Err(invalid_state("Package Export descriptor is malformed"));
        };
        if usize::try_from(*arity).ok() != Some(arguments.len()) {
            return Err(VmError::TypeError(
                "Package Export argument count does not match",
            ));
        }
        let Some(Value::Record(modules)) = manifest.get("modules") else {
            return Err(invalid_state("Package Manifest modules are malformed"));
        };
        let Some(Value::Record(module)) = modules.get(module_name) else {
            return Err(invalid_state("Package Export Module is missing"));
        };
        let Some(Value::Bytes(program_bytes)) = module.get("program") else {
            return Err(invalid_state("Package Export Program is missing"));
        };
        let program = Program::decode(program_bytes)?;
        let (entrypoint, parameters) =
            super::find_function(&program, function_name, arguments.len())?;
        let halt = program
            .tokens
            .iter()
            .position(|token| matches!(token, Token::Halt))
            .and_then(|position| u32::try_from(position).ok())
            .ok_or(VmError::TypeError(
                "Package Export Program has no halt token",
            ))?;

        let subject = owner;
        let instance = ObjectId::new();
        let program_id = ObjectId::new();
        let arguments_id = ObjectId::new();
        let resources_id = ObjectId::new();
        let subject_object = ObjectId::new();
        let process = ObjectId::new();
        let argument_ids = arguments
            .iter()
            .map(|_| ObjectId::new())
            .collect::<Vec<_>>();
        let resources = match manifest.get("resources") {
            Some(Value::Record(resources)) => Value::Record(resources.clone()),
            None => Value::Record(BTreeMap::new()),
            _ => return Err(invalid_state("Package Manifest resources are malformed")),
        };
        let (package_data, _) = self.package_data(owner, installation, None, transaction)?;
        let mut instance_request = CreateObject::new(
            CORE_PACKAGE_INSTANCE_TYPE,
            Value::Record(BTreeMap::from([
                ("owner".to_owned(), Value::Text(owner.to_string())),
                (
                    "installation".to_owned(),
                    Value::Text(installation.to_string()),
                ),
                ("package".to_owned(), Value::Text(artifact.to_string())),
                (
                    "sha256".to_owned(),
                    installation_fields
                        .get("sha256")
                        .cloned()
                        .ok_or_else(|| invalid_state("Package Installation has no SHA-256"))?,
                ),
                ("subject".to_owned(), Value::Text(subject.to_string())),
                ("program".to_owned(), Value::Text(program_id.to_string())),
                ("process".to_owned(), Value::Text(process.to_string())),
                ("export".to_owned(), Value::Text(export_name.to_owned())),
                ("status".to_owned(), Value::Text("running".to_owned())),
            ]))
            .encode()?,
        )
        .with_id(instance)
        .with_parent(installation)
        .with_link("installation", installation)
        .with_link("package", artifact)
        .with_grant(owner, Capability::Inspect)
        .with_grant(owner, Capability::ViewValue)
        .with_grant(owner, Capability::Invoke)
        .with_grant(owner, Capability::Reparent);
        instance_request.capabilities = [
            Capability::CreateChild,
            Capability::Inspect,
            Capability::Link,
            Capability::ViewValue,
            Capability::Invoke,
            Capability::Reparent,
            Capability::Retire,
        ]
        .into_iter()
        .collect();
        transaction
            .expect(installation, installation_view.header().version)
            .create(instance_request);

        let program_request = CreateObject::new(CORE_PROGRAM_TYPE, program.encode()?)
            .with_id(program_id)
            .with_parent(instance)
            .with_grant(owner, Capability::Inspect)
            .with_grant(owner, Capability::ViewValue);
        let arguments_request =
            CreateObject::new(CORE_VALUE_TYPE, Value::Array(arguments.to_vec()).encode()?)
                .with_id(arguments_id)
                .with_parent(instance)
                .with_grant(owner, Capability::Inspect)
                .with_grant(owner, Capability::ViewValue);
        let resources_request = CreateObject::new(CORE_VALUE_TYPE, resources.encode()?)
            .with_id(resources_id)
            .with_parent(instance)
            .with_grant(owner, Capability::Inspect)
            .with_grant(owner, Capability::ViewValue);
        let subject_request = CreateObject::new(
            CORE_PACKAGE_SUBJECT_TYPE,
            Value::Record(BTreeMap::from([
                ("owner".to_owned(), Value::Text(owner.to_string())),
                ("subject".to_owned(), Value::Text(subject.to_string())),
                (
                    "installation".to_owned(),
                    Value::Text(installation.to_string()),
                ),
                ("capabilities".to_owned(), Value::Array(Vec::new())),
            ]))
            .encode()?,
        )
        .with_id(subject_object)
        .with_parent(instance)
        .with_grant(owner, Capability::Inspect)
        .with_grant(owner, Capability::ViewValue);
        transaction
            .create(program_request)
            .create(arguments_request)
            .create(resources_request)
            .create(subject_request);

        let mut locals = BTreeMap::new();
        for ((parameter, argument), object) in parameters.iter().zip(arguments).zip(&argument_ids) {
            let request = CreateObject::new(CORE_VALUE_TYPE, argument.encode()?)
                .with_id(*object)
                .with_parent(instance)
                .with_grant(subject, Capability::Inspect)
                .with_grant(subject, Capability::ViewValue)
                .with_grant(subject, Capability::ReplaceValue);
            transaction.create(request);
            locals.insert(parameter.clone(), *object);
        }
        let state = ProcessState {
            program: program_id,
            subject,
            token_position: entrypoint,
            stack: Vec::new(),
            variables: BTreeMap::from([("arguments".to_owned(), arguments_id)]),
            status: ProcessStatus::Ready,
            wait_reason: WaitReason::None,
            lease_owner: None,
            lease_generation: 0,
            lease_deadline_unix_ms: None,
            result: None,
            error: None,
            ended_at_unix_ms: None,
            frames: vec![CallFrame {
                return_position: halt,
                stack_base: 0,
                locals,
                receiver: None,
                class: None,
            }],
            handlers: Vec::new(),
        };
        let mut process_request = CreateObject::new(PROCESS_TYPE, encode_process_state(&state)?)
            .with_id(process)
            .with_parent(instance)
            .with_link("program", program_id)
            .with_link("package_instance", instance)
            .with_link("package_installation", installation)
            .with_link("package_data", package_data)
            .with_link("package_resources", resources_id)
            .with_link("process", process)
            .with_grant(owner, Capability::Inspect)
            .with_grant(owner, Capability::ViewValue)
            .with_grant(owner, Capability::ReplaceValue)
            .with_grant(owner, Capability::CreateChild)
            .with_grant(owner, Capability::Invoke)
            .with_grant(owner, Capability::Link)
            .with_grant(owner, Capability::Retire);
        if let Some(console) = self.console_provider {
            process_request = process_request.with_link("console", console);
        }
        for (name, service) in &self.kernel_services {
            process_request = process_request.with_link(name.clone(), *service);
        }
        transaction
            .create(process_request)
            .set_link(instance, "program", program_id)
            .set_link(instance, "arguments", arguments_id)
            .set_link(instance, "resources", resources_id)
            .set_link(instance, "subject", subject_object)
            .set_link(instance, "process", process)
            .set_link(instance, "data", package_data);
        Ok(process)
    }

    pub(super) fn package_instance_process(
        &self,
        owner: SubjectId,
        instance: ObjectId,
    ) -> Result<ObjectId, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, instance)?;
        let value = Value::decode(view.state())?;
        let Value::Record(fields) = value else {
            return Err(invalid_state("Package Instance state is malformed"));
        };
        if view.header().type_id != CORE_PACKAGE_INSTANCE_TYPE
            || fields.get("owner") != Some(&Value::Text(owner.to_string()))
        {
            return Err(VmError::TypeError(
                "Package Instance belongs to another user",
            ));
        }
        match fields.get("process") {
            Some(Value::Text(id)) => id
                .parse::<ObjectId>()
                .map_err(|_| invalid_state("Package Instance Process ID is malformed")),
            _ => Err(invalid_state("Package Instance has no Process")),
        }
    }

    pub(super) fn package_instance_status(
        &self,
        owner: SubjectId,
        instance: ObjectId,
    ) -> Result<Value, VmError> {
        let process = self.package_instance_process(owner, instance)?;
        let status = match self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), process)
        {
            Ok(view) if view.header().type_id == PROCESS_TYPE => Value::Text(
                process_status_name(decode_process_state(view.state())?.status).to_owned(),
            ),
            _ => Value::Text("expired".to_owned()),
        };
        Ok(Value::Record(BTreeMap::from([
            ("instance".to_owned(), Value::Text(instance.to_string())),
            ("process".to_owned(), Value::Text(process.to_string())),
            ("status".to_owned(), status),
        ])))
    }

    fn package_capability_targets(&self, capability: &str) -> Result<Vec<ObjectId>, VmError> {
        let root = capability.split('.').next().unwrap_or(capability);
        let singleton = match root {
            "console" => self.console_provider,
            "time" => self.kernel_services.get("time").copied(),
            "resolver" => self.kernel_services.get("resolver").copied(),
            "math" => self.kernel_services.get("math").copied(),
            "crypto" => self.kernel_services.get("crypto").copied(),
            _ => None,
        };
        let objects = if matches!(root, "console" | "time" | "resolver" | "math" | "crypto") {
            singleton.map(|object| vec![object]).ok_or_else(|| {
                VmError::Provider(format!("requested capability '{root}' is unavailable"))
            })?
        } else {
            let type_id = match root {
                "keyboard" => DEVICE_KEYBOARD_TYPE,
                "display" => DEVICE_DISPLAY_TYPE,
                "sensor" => DEVICE_SENSOR_TYPE,
                "storage" => {
                    return Err(VmError::TypeError(
                        "Package Applications cannot access raw block storage without a scoped storage Provider",
                    ));
                }
                "network" => {
                    return Err(VmError::TypeError(
                        "Package Applications cannot access shared Network Endpoints; a per-Application Endpoint Provider is required",
                    ));
                }
                _ => {
                    return Err(VmError::TypeError(
                        "unsupported Package capability; use console, time, keyboard, display, sensor, resolver, math or crypto",
                    ));
                }
            };
            let objects = self.manager.query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(type_id),
            )?;
            if objects.is_empty() {
                return Err(VmError::Provider(format!(
                    "requested capability '{root}' has no discovered Object"
                )));
            }
            objects.into_iter().map(|object| object.id).collect()
        };
        if let Some((_, method)) = capability.split_once('.') {
            for object in &objects {
                let type_id = self
                    .manager
                    .inspect(AccessContext::new(SYSTEM_SUBJECT), *object)?
                    .type_id;
                if !self
                    .manager
                    .type_by_id(type_id)?
                    .domain_capabilities
                    .contains(method)
                {
                    return Err(VmError::TypeError(
                        "Package capability names a method that the Object does not provide",
                    ));
                }
            }
        }
        Ok(objects)
    }

    pub(super) fn enforce_package_capability(
        &self,
        process: ObjectId,
        subject: SubjectId,
        target_type: TypeId,
        method: &str,
    ) -> Result<(), VmError> {
        let root = match target_type {
            CORE_CONSOLE_TYPE => "console",
            CORE_TIME_TYPE => "time",
            CORE_MATH_TYPE => "math",
            CORE_CRYPTO_TYPE => "crypto",
            NET_RESOLVER_TYPE => "resolver",
            NET_ENDPOINT_TYPE => "network",
            DEVICE_KEYBOARD_TYPE => "keyboard",
            DEVICE_DISPLAY_TYPE => "display",
            DEVICE_SENSOR_TYPE => "sensor",
            DEVICE_BLOCK_STORAGE_TYPE => "storage",
            _ => return Ok(()),
        };
        let process_view = self.manager.read(self.context, process)?;
        let Some(package_subject) = process_view.links().get("package_subject").copied() else {
            return Ok(());
        };
        let subject_value = self.manager.value(self.context, package_subject)?;
        let Value::Record(fields) = subject_value else {
            return Err(invalid_state("Package Subject state is malformed"));
        };
        if fields.get("subject") != Some(&Value::Text(subject.to_string())) {
            return Err(invalid_state(
                "Process Subject does not match its Package Subject",
            ));
        }
        // Library exports deliberately execute as their caller. They carry a
        // Package Subject record for provenance, but do not create a sandbox.
        if fields.get("owner") == Some(&Value::Text(subject.to_string())) {
            return Ok(());
        }
        let Some(Value::Array(capabilities)) = fields.get("capabilities") else {
            return Err(invalid_state("Package Subject capabilities are malformed"));
        };
        let granted = capabilities.iter().any(|capability| {
            let Value::Text(capability) = capability else {
                return false;
            };
            capability == root
                || capability
                    .strip_prefix(&format!("{root}."))
                    .is_some_and(|allowed_method| allowed_method == method)
        });
        if granted {
            Ok(())
        } else {
            Err(VmError::Provider(format!(
                "Package capability '{root}.{method}' was not granted"
            )))
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "uninstall validates reverse references before atomic retirement"
    )]
    pub(super) fn uninstall_package(
        &self,
        process: ObjectId,
        process_state: &mut ProcessState,
        owner: SubjectId,
        installation: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let installation_view = self.manager.read(system, installation)?;
        if installation_view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE {
            return Err(VmError::TypeError("Object is not a Package Installation"));
        }
        let value = Value::decode(installation_view.state())?;
        if !package_installation_owned(&value, owner) {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        let parent = installation_view
            .header()
            .parent_id
            .ok_or_else(|| invalid_state("Package Installation has no user index"))?;
        let index_view = self.manager.read(system, parent)?;
        let Value::Record(fields) = value else {
            return Err(invalid_state("Package Installation is malformed"));
        };
        let coordinate = match fields.get("coordinate") {
            Some(Value::Text(coordinate)) => coordinate.clone(),
            _ => return Err(invalid_state("Package Installation has no coordinate")),
        };
        if index_view.links().get(&package_user_link_key(&coordinate)) != Some(&installation) {
            return Err(invalid_state(
                "Package Installation is absent from its user index",
            ));
        }
        for (link, dependent) in installation_view.links() {
            if !link.starts_with("required_by:") {
                continue;
            }
            let dependent_view = self.manager.read(system, *dependent)?;
            if dependent_view.header().type_id == CORE_PACKAGE_INSTALLATION_TYPE
                && package_installation_owned(&Value::decode(dependent_view.state())?, owner)
            {
                return Err(VmError::Provider(
                    "Package is required by another installed Package; uninstall dependents first"
                        .to_owned(),
                ));
            }
        }
        for child in installation_view.children() {
            let module_view = self.manager.read(system, *child)?;
            if module_view.header().type_id == CORE_PACKAGE_INSTANCE_TYPE {
                let Ok(Value::Record(instance)) = Value::decode(module_view.state()) else {
                    continue;
                };
                let Some(Value::Text(process_id)) = instance.get("process") else {
                    continue;
                };
                let Ok(process_id) = process_id.parse::<ObjectId>() else {
                    continue;
                };
                let Ok(process_view) = self.manager.read(system, process_id) else {
                    continue;
                };
                if process_view.header().type_id == PROCESS_TYPE {
                    let state = decode_process_state(process_view.state())?;
                    if matches!(
                        state.status,
                        ProcessStatus::Running
                            | ProcessStatus::Ready
                            | ProcessStatus::Waiting
                            | ProcessStatus::Suspended
                    ) {
                        return Err(VmError::Provider(
                            "Package has an active Application Process".to_owned(),
                        ));
                    }
                }
                continue;
            }
            if module_view.header().type_id != CORE_PACKAGE_MODULE_TYPE {
                continue;
            }
            for (link_name, instance) in module_view.links() {
                if !link_name.starts_with("instance:") {
                    continue;
                }
                let instance_view = self.manager.read(system, *instance)?;
                if instance_view.header().type_id == CORE_MODULE_INSTANCE_TYPE
                    && matches!(
                        Value::decode(instance_view.state())?,
                        Value::Record(ref instance_fields)
                            if instance_fields.get("status")
                                == Some(&Value::Text("active".to_owned()))
                    )
                {
                    return Err(VmError::Provider(
                        "Package Module is loaded in an active Terminal Session".to_owned(),
                    ));
                }
            }
        }
        let mut retired = self.package_auto_remove_set(owner, installation)?;
        retired.insert(installation);
        for package in &retired {
            self.stage_package_retirement_state(owner, *package, transaction)?;
        }
        self.retire_objects_as(
            process,
            process_state,
            retired,
            AccessContext::new(SYSTEM_SUBJECT),
            transaction,
        )
    }

    /// Finds implicit dependencies that cease to have a root after one Package
    /// is retired.  A direct user install is explicit and is never selected;
    /// neither is a dependency still referenced by another Package or loaded
    /// by a terminal/application instance.  The fixed point preserves chains:
    /// once `editor` goes away, its private `syntax` dependency and then
    /// `syntax`'s private `parser` dependency can be collected together.
    fn package_auto_remove_set(
        &self,
        owner: SubjectId,
        root: ObjectId,
    ) -> Result<BTreeSet<ObjectId>, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let mut reachable = BTreeSet::new();
        let mut pending = vec![root];
        while let Some(package) = pending.pop() {
            if !reachable.insert(package) {
                continue;
            }
            let view = self.manager.read(system, package)?;
            if view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
                || !package_installation_owned(&Value::decode(view.state())?, owner)
            {
                return Err(invalid_state(
                    "Package dependency does not belong to the uninstalling user",
                ));
            }
            pending.extend(
                view.links()
                    .iter()
                    .filter(|(name, _)| name.starts_with("dependency:"))
                    .map(|(_, dependency)| *dependency),
            );
        }

        let mut removable = BTreeSet::from([root]);
        let mut changed = true;
        while changed {
            changed = false;
            for package in &reachable {
                if removable.contains(package)
                    || !self.package_is_implicit_and_quiet(owner, *package)?
                {
                    continue;
                }
                let view = self.manager.read(system, *package)?;
                let only_retiring_dependents = view
                    .links()
                    .iter()
                    .filter(|(name, _)| name.starts_with("required_by:"))
                    .all(|(_, dependent)| removable.contains(dependent));
                if only_retiring_dependents {
                    removable.insert(*package);
                    changed = true;
                }
            }
        }
        removable.remove(&root);
        Ok(removable)
    }

    fn package_is_implicit_and_quiet(
        &self,
        owner: SubjectId,
        installation: ObjectId,
    ) -> Result<bool, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, installation)?;
        let value = Value::decode(view.state())?;
        let Value::Record(fields) = value else {
            return Ok(false);
        };
        // Old development-state installs did not record this field.  Treating
        // them as explicit is conservative: an upgrade never deletes a
        // package merely because it cannot prove that it was implicit.
        if view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE
            || !package_installation_owned(&Value::Record(fields.clone()), owner)
            || fields.get("explicit") != Some(&Value::Bool(false))
        {
            return Ok(false);
        }
        for child in view.children() {
            let child_view = self.manager.read(system, *child)?;
            if child_view.header().type_id == CORE_PACKAGE_INSTANCE_TYPE {
                if let Value::Record(instance) = Value::decode(child_view.state())? {
                    if let Some(Value::Text(process)) = instance.get("process") {
                        if let Ok(process) = process.parse::<ObjectId>() {
                            if let Ok(process_view) = self.manager.read(system, process) {
                                if process_view.header().type_id == PROCESS_TYPE
                                    && matches!(
                                        decode_process_state(process_view.state())?.status,
                                        ProcessStatus::Running
                                            | ProcessStatus::Ready
                                            | ProcessStatus::Waiting
                                            | ProcessStatus::Suspended
                                    )
                                {
                                    return Ok(false);
                                }
                            }
                        }
                    }
                }
                continue;
            }
            if child_view.header().type_id != CORE_PACKAGE_MODULE_TYPE {
                continue;
            }
            if child_view
                .links()
                .keys()
                .any(|name| name.starts_with("instance:"))
            {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn stage_package_retirement_state(
        &self,
        owner: SubjectId,
        installation: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<(), VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let view = self.manager.read(system, installation)?;
        if view.header().type_id != CORE_PACKAGE_INSTALLATION_TYPE {
            return Err(VmError::TypeError("Object is not a Package Installation"));
        }
        let Value::Record(mut fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Package Installation is malformed"));
        };
        if !package_installation_owned(&Value::Record(fields.clone()), owner) {
            return Err(VmError::TypeError(
                "Package Installation belongs to another user",
            ));
        }
        let data = self.package_data_object(installation)?;
        let (has_data, snapshot) = if let Some(data) = data {
            (true, self.manager.value(system, data)?)
        } else {
            (false, Value::Null)
        };
        fields.insert("status".to_owned(), Value::Text("retired".to_owned()));
        fields.insert("data_present".to_owned(), Value::Bool(has_data));
        fields.insert("data_snapshot".to_owned(), snapshot);
        transaction
            .expect(installation, view.header().version)
            .update_state(installation, Value::Record(fields).encode()?);
        Ok(())
    }

    pub(super) fn package_registry_object(&self) -> Result<ObjectId, VmError> {
        let Some(registry) = self.kernel_services.get("packages").copied().or_else(|| {
            self.manager
                .query(
                    AccessContext::new(SYSTEM_SUBJECT),
                    &ObjectQuery::new().with_type(CORE_PACKAGE_REGISTRY_TYPE),
                )
                .ok()?
                .first()
                .map(|header| header.id)
        }) else {
            return Err(VmError::Provider(
                "Package Registry is unavailable".to_owned(),
            ));
        };
        Ok(registry)
    }

    fn package_user_index(
        &self,
        owner: SubjectId,
        registry: ObjectId,
        registry_view: &ObjectView,
        transaction: &mut Transaction,
    ) -> Result<(ObjectId, ObjectVersion, bool), VmError> {
        if let Some(index) = registry_view
            .links()
            .get(&package_user_index_key(owner))
            .copied()
        {
            let view = self
                .manager
                .read(AccessContext::new(SYSTEM_SUBJECT), index)?;
            if view.header().type_id != CORE_NAMESPACE_TYPE
                || !package_user_index_matches(&Value::decode(view.state())?, owner)
            {
                return Err(invalid_state("Package user index is malformed"));
            }
            return Ok((index, view.header().version, false));
        }

        let index = ObjectId::new();
        let value = Value::Record(BTreeMap::from([
            ("name".to_owned(), Value::Text(format!("packages:{owner}"))),
            ("owner".to_owned(), Value::Text(owner.to_string())),
        ]));
        transaction
            .expect(registry, registry_view.header().version)
            .create(
                CreateObject::new(CORE_NAMESPACE_TYPE, value.encode()?)
                    .with_id(index)
                    .with_parent(registry)
                    .with_grant(owner, Capability::Reparent),
            )
            .set_link(registry, package_user_index_key(owner), index);
        Ok((index, ObjectVersion::default(), true))
    }
}
