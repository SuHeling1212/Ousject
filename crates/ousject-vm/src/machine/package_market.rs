#![allow(clippy::wildcard_imports)]

use super::package_support::*;
use super::*;
use serde::Deserialize;
use std::io::Read;

const MAX_MARKET_INDEX_BYTES: usize = 8 * 1024 * 1024;
const MAX_MARKET_PACKAGE_BYTES: usize = 64 * 1024 * 1024;
const MAX_PACKAGE_DOWNLOAD_OBJECT_BYTES: usize = 15 * 1024 * 1024;
const MARKET_CHUNK_BYTES: usize = 4 * 1024 * 1024;
const MAX_MARKET_RESULTS: usize = 100;
const MAX_MARKET_RECORDS: usize = 10_000;

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct MarketIndex {
    api_version: String,
    packages: Vec<MarketRecord>,
}

#[derive(Debug, Deserialize)]
struct MarketRecord {
    namespace: String,
    name: String,
    version: String,
    kind: String,
    coordinate: String,
    sha256: String,
    download: String,
    bytes: u64,
    #[serde(default)]
    dependencies: Vec<MarketDependency>,
    #[serde(default)]
    summary: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    tags: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct MarketDependency {
    sha256: String,
    optional: bool,
}

impl VirtualMachine {
    pub(super) fn market_invoke(
        &self,
        owner: SubjectId,
        market: ObjectId,
        method: &str,
        arguments: &[Value],
        transaction: &mut Transaction,
    ) -> Result<Value, VmError> {
        match (method, arguments) {
            ("configure", [Value::Text(origin)]) => {
                self.configure_market(owner, market, origin, transaction)
            }
            ("origin", []) => Ok(self
                .market_config(owner, market)?
                .map_or(Value::Null, |(_, _, fields)| {
                    fields.get("origin").cloned().unwrap_or(Value::Null)
                })),
            ("update", []) => self.update_market_index(owner, market, transaction),
            ("search", [Value::Text(query)]) => self.search_market(owner, market, query),
            ("info", [Value::Text(sha256)]) => self.market_info(owner, market, sha256),
            ("download", [Value::Text(sha256)]) => {
                let (_, config, _) = self.require_market_config(owner, market)?;
                let id = self.download_market_package(owner, config.header().id, sha256)?;
                Ok(Value::Text(id.to_string()))
            }
            ("install", [Value::Text(sha256)]) => {
                let (_, config, _) = self.require_market_config(owner, market)?;
                let mut visiting = BTreeSet::new();
                let mut imported = BTreeMap::new();
                let artifact = self.install_market_closure(
                    owner,
                    config.header().id,
                    sha256,
                    0,
                    &mut visiting,
                    &mut imported,
                )?;
                let installation = self.install_package(
                    owner,
                    self.package_registry_object()?,
                    artifact,
                    transaction,
                )?;
                Ok(Value::Text(installation.to_string()))
            }
            ("list", []) => self.market_installed(owner),
            ("bytes", []) => Err(VmError::TypeError(
                "market.bytes must be called on a Package Download Object",
            )),
            _ => Err(VmError::TypeError("invalid Market capability or arguments")),
        }
    }

    pub(super) fn package_download_invoke(
        &self,
        owner: SubjectId,
        download: ObjectId,
        method: &str,
    ) -> Result<Value, VmError> {
        let view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), download)?;
        if view.header().type_id != CORE_PACKAGE_DOWNLOAD_TYPE {
            return Err(VmError::TypeError("Object is not a Package Download"));
        }
        let Value::Record(fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Package Download is malformed"));
        };
        if fields.get("owner") != Some(&Value::Text(owner.to_string())) {
            return Err(VmError::TypeError(
                "Package Download belongs to another user",
            ));
        }
        match method {
            "bytes" => {
                if fields.get("status") != Some(&Value::Text("complete".to_owned())) {
                    return Err(VmError::Provider(
                        "Package Download is incomplete; resume it with market.download(sha256)"
                            .to_owned(),
                    ));
                }
                let (
                    Some(Value::Bytes(bytes)),
                    Some(Value::Text(sha256)),
                    Some(Value::Integer(size)),
                ) = (fields.get("data"), fields.get("sha256"), fields.get("size"))
                else {
                    return Err(invalid_state("completed Package Download is malformed"));
                };
                if usize::try_from(*size).ok() != Some(bytes.len())
                    || package_sha256(bytes) != *sha256
                {
                    return Err(invalid_state(
                        "Package Download content failed SHA validation",
                    ));
                }
                Ok(Value::Bytes(bytes.clone()))
            }
            "info" => Ok(Value::Record(BTreeMap::from([
                (
                    "sha256".to_owned(),
                    fields.get("sha256").cloned().unwrap_or(Value::Null),
                ),
                (
                    "bytes".to_owned(),
                    fields.get("size").cloned().unwrap_or(Value::Null),
                ),
                (
                    "received".to_owned(),
                    fields
                        .get("data")
                        .and_then(|value| match value {
                            Value::Bytes(data) => Some(Value::Integer(
                                i64::try_from(data.len()).unwrap_or(i64::MAX),
                            )),
                            _ => None,
                        })
                        .unwrap_or(Value::Integer(0)),
                ),
                (
                    "status".to_owned(),
                    fields.get("status").cloned().unwrap_or(Value::Null),
                ),
            ]))),
            _ => Err(VmError::TypeError("unknown Package Download capability")),
        }
    }

    pub(super) fn export_package_artifact(&self, package: ObjectId) -> Result<Value, VmError> {
        if self
            .manager
            .inspect(AccessContext::new(SYSTEM_SUBJECT), package)?
            .type_id
            != CORE_PACKAGE_TYPE
        {
            return Err(VmError::TypeError("Object is not a Package"));
        }
        if !self.verify_package_artifact(package)? {
            return Err(VmError::TypeError("Package validation failed"));
        }
        let value = self
            .manager
            .value(AccessContext::new(SYSTEM_SUBJECT), package)?;
        let Value::Record(fields) = value else {
            return Err(invalid_state("Package is malformed"));
        };
        let Some(Value::Record(manifest)) = fields.get("manifest") else {
            return Err(invalid_state("Package has no Manifest"));
        };
        Ok(Value::Bytes(Value::Record(manifest.clone()).encode()?))
    }

    pub(super) fn import_package_artifact(
        &self,
        bytes: &[u8],
        expected_hash: Option<&str>,
    ) -> Result<ObjectId, VmError> {
        if bytes.is_empty() || bytes.len() > MAX_PACKAGE_DOWNLOAD_OBJECT_BYTES {
            return Err(VmError::TypeError(
                "Package must be between 1 byte and 15 MiB",
            ));
        }
        let digest = package_sha256(bytes);
        if expected_hash.is_some_and(|expected| expected != digest) {
            return Err(VmError::TypeError(
                "downloaded Package SHA-256 does not match the requested content",
            ));
        }
        let Value::Record(manifest) = Value::decode(bytes)? else {
            return Err(VmError::TypeError("Package must contain a Manifest Record"));
        };
        if Value::Record(manifest.clone()).encode()?.as_slice() != bytes {
            return Err(VmError::TypeError("Package is not canonically encoded"));
        }
        let coordinate = package_coordinate(&manifest)?;
        let dependencies = parse_package_dependencies(manifest.get("dependencies"))?;
        let registry = self.package_registry_object()?;
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let registry_view = self.manager.read(system, registry)?;
        for dependency in &dependencies {
            let Some(artifact) = registry_view
                .links()
                .get(&format!("coordinate:{}", dependency.coordinate))
                .copied()
            else {
                return Err(VmError::MissingKey(dependency.coordinate.clone()));
            };
            let Value::Record(fields) = self.manager.value(system, artifact)? else {
                return Err(invalid_state("Package dependency Package is malformed"));
            };
            if fields.get("sha256") != Some(&Value::Text(dependency.sha256.clone())) {
                return Err(VmError::TypeError(
                    "Package dependency does not match its exact SHA-256",
                ));
            }
        }

        let artifact = ObjectId::new();
        let mut create = self.manager.begin(system);
        create
            .expect(registry, registry_view.header().version)
            .create(
                CreateObject::new(
                    CORE_PACKAGE_TYPE,
                    Value::Record(BTreeMap::from([
                        ("manifest".to_owned(), Value::Record(manifest)),
                        ("sha256".to_owned(), Value::Text(digest.clone())),
                        ("status".to_owned(), Value::Text("verified".to_owned())),
                    ]))
                    .encode()?,
                )
                .with_id(artifact)
                .with_parent(registry),
            );
        self.manager.commit(create)?;

        if !self.verify_package_artifact(artifact)? {
            self.retire_unindexed_package_artifact(registry, artifact)?;
            return Err(VmError::TypeError("Package validation failed"));
        }

        let current_registry = self.manager.read(system, registry)?;
        let coordinate_key = format!("coordinate:{coordinate}");
        let hash_key = format!("package:{digest}");
        if let Some(existing) = current_registry.links().get(&coordinate_key).copied() {
            self.retire_unindexed_package_artifact(registry, artifact)?;
            if self.verify_package_artifact(existing)?
                && artifact_manifest_hash(&self.manager.value(system, existing)?)? == digest
            {
                return Ok(existing);
            }
            return Err(VmError::Provider(
                "Package coordinate is already bound to different content".to_owned(),
            ));
        }
        if current_registry.links().contains_key(&hash_key) {
            self.retire_unindexed_package_artifact(registry, artifact)?;
            return Err(VmError::Provider(
                "Package SHA-256 is already registered under another coordinate".to_owned(),
            ));
        }
        let mut publish = self.manager.begin(system);
        publish
            .expect(registry, current_registry.header().version)
            .set_link(registry, hash_key, artifact)
            .set_link(registry, coordinate_key, artifact);
        self.manager.commit(publish)?;
        Ok(artifact)
    }

    fn retire_unindexed_package_artifact(
        &self,
        registry: ObjectId,
        package: ObjectId,
    ) -> Result<(), VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let artifact_view = self.manager.read(system, package)?;
        let registry_view = self.manager.read(system, registry)?;
        let mut transaction = self.manager.begin(system);
        transaction
            .expect(package, artifact_view.header().version)
            .expect(registry, registry_view.header().version)
            .tombstone(package);
        self.manager.commit(transaction)?;
        Ok(())
    }

    fn configure_market(
        &self,
        owner: SubjectId,
        market: ObjectId,
        origin: &str,
        transaction: &mut Transaction,
    ) -> Result<Value, VmError> {
        let origin = normalize_market_origin(origin)?;
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let root = self.manager.read(system, market)?;
        let key = market_user_link_key(owner);
        if let Some(config) = root.links().get(&key).copied() {
            let view = self.manager.read(system, config)?;
            let Value::Record(mut fields) = Value::decode(view.state())? else {
                return Err(invalid_state("Market configuration is malformed"));
            };
            if fields.get("owner") != Some(&Value::Text(owner.to_string())) {
                return Err(invalid_state("Market configuration owner is inconsistent"));
            }
            fields.insert("origin".to_owned(), Value::Text(origin.clone()));
            fields.insert("index".to_owned(), Value::Null);
            fields.insert("updated_at_unix_ms".to_owned(), Value::Null);
            transaction
                .expect(config, view.header().version)
                .update_state(config, Value::Record(fields).encode()?);
        } else {
            let config = ObjectId::new();
            let value = Value::Record(BTreeMap::from([
                ("owner".to_owned(), Value::Text(owner.to_string())),
                ("origin".to_owned(), Value::Text(origin.clone())),
                ("index".to_owned(), Value::Null),
                ("updated_at_unix_ms".to_owned(), Value::Null),
            ]));
            transaction
                .expect(market, root.header().version)
                .create(
                    CreateObject::new(CORE_PACKAGE_MARKET_CONFIG_TYPE, value.encode()?)
                        .with_id(config)
                        .with_parent(market),
                )
                .set_link(market, key, config);
        }
        Ok(Value::Text(origin))
    }

    fn market_config(
        &self,
        owner: SubjectId,
        market: ObjectId,
    ) -> Result<Option<MarketConfiguration>, VmError> {
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let root = self.manager.read(system, market)?;
        if root.header().type_id != CORE_PACKAGE_MARKET_TYPE {
            return Err(VmError::TypeError("Object is not the Package Market"));
        }
        let Some(config) = root.links().get(&market_user_link_key(owner)).copied() else {
            return Ok(None);
        };
        let view = self.manager.read(system, config)?;
        if view.header().type_id != CORE_PACKAGE_MARKET_CONFIG_TYPE {
            return Err(invalid_state("Market configuration has the wrong Type"));
        }
        let Value::Record(fields) = Value::decode(view.state())? else {
            return Err(invalid_state("Market configuration is malformed"));
        };
        if fields.get("owner") != Some(&Value::Text(owner.to_string())) {
            return Err(invalid_state("Market configuration owner is inconsistent"));
        }
        Ok(Some((config, view, fields)))
    }

    fn require_market_config(
        &self,
        owner: SubjectId,
        market: ObjectId,
    ) -> Result<(String, ObjectView, BTreeMap<String, Value>), VmError> {
        let (_, view, fields) = self.market_config(owner, market)?.ok_or_else(|| {
            VmError::Provider("configure a Market first with market.configure(origin)".to_owned())
        })?;
        let origin = match fields.get("origin") {
            Some(Value::Text(origin)) => origin.clone(),
            _ => {
                return Err(VmError::Provider(
                    "configure a Market first with market.configure(origin)".to_owned(),
                ));
            }
        };
        Ok((origin, view, fields))
    }

    fn update_market_index(
        &self,
        owner: SubjectId,
        market: ObjectId,
        transaction: &mut Transaction,
    ) -> Result<Value, VmError> {
        let (origin, view, mut fields) = self.require_market_config(owner, market)?;
        let url = format!("{origin}/market/v1/index.json");
        let bytes = market_http_get(&url, MAX_MARKET_INDEX_BYTES)?;
        let index: MarketIndex = serde_json::from_slice(&bytes)
            .map_err(|error| VmError::Provider(format!("Market index is invalid: {error}")))?;
        if index.api_version != "cilexec.market/v1" || index.packages.len() > MAX_MARKET_RECORDS {
            return Err(VmError::TypeError("unsupported or oversized Market index"));
        }
        let mut seen_hashes = BTreeSet::new();
        let mut seen_coordinates = BTreeSet::new();
        let mut records = Vec::with_capacity(index.packages.len());
        for record in index.packages {
            validate_coordinate(&record.namespace, "namespace")?;
            validate_coordinate(&record.name, "name")?;
            validate_release(&record.version)?;
            let coordinate = format!("{}/{}/{}", record.namespace, record.name, record.version);
            let size = usize::try_from(record.bytes)
                .map_err(|_| VmError::TypeError("Market Package size is too large"))?;
            let mut dependency_hashes = BTreeSet::new();
            let invalid_dependencies = record.dependencies.len() > MAX_PACKAGE_DEPENDENCIES
                || record.dependencies.iter().any(|dependency| {
                    !valid_sha256(&dependency.sha256)
                        || !dependency_hashes.insert(dependency.sha256.clone())
                });
            if record.coordinate != coordinate
                || record.download != format!("/market/v1/{}", record.sha256)
                || !valid_sha256(&record.sha256)
                || size == 0
                || size > MAX_MARKET_PACKAGE_BYTES
                || !matches!(record.kind.as_str(), "application" | "library")
                || invalid_dependencies
                || !seen_hashes.insert(record.sha256.clone())
                || !seen_coordinates.insert(coordinate.clone())
                || record.summary.len() > 4096
                || record.description.len() > 16_384
                || record.tags.len() > 64
                || record.tags.iter().any(|tag| tag.len() > 128)
            {
                return Err(VmError::TypeError(
                    "Market index has an invalid Package record",
                ));
            }
            let bytes = i64::try_from(record.bytes)
                .map_err(|_| VmError::TypeError("Market Package size is out of range"))?;
            records.push(Value::Record(BTreeMap::from([
                ("namespace".to_owned(), Value::Text(record.namespace)),
                ("name".to_owned(), Value::Text(record.name)),
                ("version".to_owned(), Value::Text(record.version)),
                ("kind".to_owned(), Value::Text(record.kind)),
                ("coordinate".to_owned(), Value::Text(coordinate)),
                ("sha256".to_owned(), Value::Text(record.sha256)),
                ("download".to_owned(), Value::Text(record.download)),
                ("bytes".to_owned(), Value::Integer(bytes)),
                (
                    "dependencies".to_owned(),
                    Value::Array(
                        record
                            .dependencies
                            .into_iter()
                            .map(|dependency| {
                                Value::Record(BTreeMap::from([
                                    ("sha256".to_owned(), Value::Text(dependency.sha256)),
                                    ("optional".to_owned(), Value::Bool(dependency.optional)),
                                ]))
                            })
                            .collect(),
                    ),
                ),
                ("summary".to_owned(), Value::Text(record.summary)),
                ("description".to_owned(), Value::Text(record.description)),
                (
                    "tags".to_owned(),
                    Value::Array(record.tags.into_iter().map(Value::Text).collect()),
                ),
            ])));
        }
        let count = i64::try_from(records.len())
            .map_err(|_| VmError::TypeError("Market Package count is out of range"))?;
        fields.insert("index".to_owned(), Value::Array(records));
        fields.insert(
            "updated_at_unix_ms".to_owned(),
            Value::Integer(
                i64::try_from(unix_time_millis())
                    .map_err(|_| invalid_state("Market update time is out of range"))?,
            ),
        );
        let config = view.header().id;
        transaction
            .expect(config, view.header().version)
            .update_state(config, Value::Record(fields).encode()?);
        Ok(Value::Record(BTreeMap::from([(
            "packages".to_owned(),
            Value::Integer(count),
        )])))
    }

    fn search_market(
        &self,
        owner: SubjectId,
        market: ObjectId,
        query: &str,
    ) -> Result<Value, VmError> {
        if query.len() > 512 {
            return Err(VmError::TypeError("Market search query is too long"));
        }
        let (_, _, fields) = self.require_market_config(owner, market)?;
        let Some(Value::Array(records)) = fields.get("index") else {
            return Err(VmError::Provider(
                "run market.update() before searching".to_owned(),
            ));
        };
        let terms = query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect::<Vec<_>>();
        let mut results = Vec::new();
        for record in records {
            if !market_record_matches(record, &terms)? {
                continue;
            }
            results.push(record.clone());
            if results.len() == MAX_MARKET_RESULTS {
                break;
            }
        }
        Ok(Value::Array(results))
    }

    fn market_info(
        &self,
        owner: SubjectId,
        market: ObjectId,
        sha256: &str,
    ) -> Result<Value, VmError> {
        if !valid_sha256(sha256) {
            return Err(VmError::TypeError("Package ID must be a lowercase SHA-256"));
        }
        let (_, _, fields) = self.require_market_config(owner, market)?;
        let Some(Value::Array(records)) = fields.get("index") else {
            return Err(VmError::Provider(
                "run market.update() before searching".to_owned(),
            ));
        };
        Ok(records
            .iter()
            .find(|record| {
                matches!(record, Value::Record(fields)
                    if fields.get("sha256") == Some(&Value::Text(sha256.to_owned())))
            })
            .cloned()
            .unwrap_or(Value::Null))
    }

    fn market_installed(&self, owner: SubjectId) -> Result<Value, VmError> {
        let installations = self.installed_packages(owner)?;
        let Value::Array(installations) = installations else {
            return Err(invalid_state("Package installation list is malformed"));
        };
        let mut result = Vec::with_capacity(installations.len());
        for id in installations {
            let Value::Text(id) = id else {
                continue;
            };
            let id = id
                .parse::<ObjectId>()
                .map_err(|_| invalid_state("Package Installation ID is malformed"))?;
            let info = self.package_installation_info(owner, id)?;
            let Value::Record(fields) = info else {
                return Err(invalid_state("Package Installation info is malformed"));
            };
            let Value::Record(installation) =
                fields.get("installation").cloned().unwrap_or(Value::Null)
            else {
                return Err(invalid_state("Package Installation record is malformed"));
            };
            result.push(Value::Record(BTreeMap::from([
                (
                    "coordinate".to_owned(),
                    installation
                        .get("coordinate")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
                (
                    "sha256".to_owned(),
                    installation.get("sha256").cloned().unwrap_or(Value::Null),
                ),
                ("installation".to_owned(), Value::Text(id.to_string())),
            ])));
        }
        Ok(Value::Array(result))
    }

    // Resume, validate, and persist each range before publishing the completed
    // SHA-addressed artifact; this is one bounded transfer state machine.
    #[expect(
        clippy::too_many_lines,
        reason = "range download integrity depends on ordered state transitions"
    )]
    fn download_market_package(
        &self,
        owner: SubjectId,
        config: ObjectId,
        sha256: &str,
    ) -> Result<ObjectId, VmError> {
        if !valid_sha256(sha256) {
            return Err(VmError::TypeError("Package ID must be a lowercase SHA-256"));
        }
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let config_view = self.manager.read(system, config)?;
        let Value::Record(config_fields) = Value::decode(config_view.state())? else {
            return Err(invalid_state("Market configuration is malformed"));
        };
        if config_fields.get("owner") != Some(&Value::Text(owner.to_string())) {
            return Err(VmError::TypeError(
                "Market configuration belongs to another user",
            ));
        }
        let record = market_index_record(&config_fields, sha256)?;
        let size = match record.get("bytes") {
            Some(Value::Integer(size)) => usize::try_from(*size)
                .map_err(|_| VmError::TypeError("Market Package size is invalid"))?,
            _ => return Err(invalid_state("Market index Package size is missing")),
        };
        if size > MAX_PACKAGE_DOWNLOAD_OBJECT_BYTES {
            return Err(VmError::TypeError(
                "Market Package exceeds Ousject's 15 MiB single-object transfer limit",
            ));
        }
        let Some(Value::Text(origin)) = config_fields.get("origin") else {
            return Err(invalid_state("Market origin is missing"));
        };
        let completed_key = format!("download:{sha256}");
        if let Some(existing) = config_view.links().get(&completed_key).copied() {
            let value = self.manager.value(system, existing)?;
            if let Value::Record(fields) = value {
                if fields.get("status") == Some(&Value::Text("complete".to_owned()))
                    && fields.get("sha256") == Some(&Value::Text(sha256.to_owned()))
                    && matches!(fields.get("data"), Some(Value::Bytes(bytes)) if bytes.len() == size && package_sha256(bytes) == sha256)
                {
                    return Ok(existing);
                }
            }
        }

        let partial_key = format!("partial:{sha256}");
        let Some(Value::Text(download_path)) = record.get("download") else {
            return Err(invalid_state("Market download path is missing"));
        };
        let url = format!("{origin}{download_path}");
        let mut existing_id = config_view.links().get(&partial_key).copied();
        let mut data = Vec::new();
        let mut etag = None;
        if let Some(id) = existing_id {
            let value = self.manager.value(system, id)?;
            if let Value::Record(fields) = value {
                if fields.get("sha256") == Some(&Value::Text(sha256.to_owned()))
                    && fields.get("size")
                        == Some(&Value::Integer(i64::try_from(size).map_err(|_| {
                            invalid_state("Market Package size is out of range")
                        })?))
                {
                    if let Some(Value::Bytes(bytes)) = fields.get("data") {
                        data.clone_from(bytes);
                    }
                    if let Some(Value::Text(value)) = fields.get("etag") {
                        etag = Some(value.clone());
                    }
                }
            }
        }
        if data.len() > size || data.len() > MAX_PACKAGE_DOWNLOAD_OBJECT_BYTES {
            data.clear();
            etag = None;
        }

        while data.len() < size {
            let offset = data.len();
            let end = offset
                .saturating_add(MARKET_CHUNK_BYTES)
                .min(size)
                .saturating_sub(1);
            let response = market_http_range(&url, offset, end, etag.as_deref(), size)?;
            if response.status == 200 && offset > 0 {
                data.clear();
                etag = None;
                self.persist_market_download_chunk(MarketDownloadChunk {
                    owner,
                    config,
                    partial_key: &partial_key,
                    sha256,
                    size,
                    data: &data,
                    etag: etag.as_deref(),
                    object: &mut existing_id,
                })?;
                continue;
            }
            if response.status != 206 && !(response.status == 200 && offset == 0) {
                return Err(VmError::Provider(format!(
                    "Market download returned HTTP status {}",
                    response.status
                )));
            }
            if response.status == 200 {
                if response.bytes.len() != size {
                    return Err(VmError::Provider(
                        "Market server ignored a Range request for a nonmatching file size"
                            .to_owned(),
                    ));
                }
                data = response.bytes;
            } else {
                let expected = end - offset + 1;
                if response.bytes.len() != expected
                    || response.content_range.as_deref()
                        != Some(format!("bytes {offset}-{end}/{size}").as_str())
                {
                    return Err(VmError::Provider(
                        "Market server returned an invalid byte range".to_owned(),
                    ));
                }
                if etag.is_some() && response.etag.is_some() && etag != response.etag {
                    data.clear();
                    etag = None;
                    continue;
                }
                if etag.is_none() {
                    etag.clone_from(&response.etag);
                }
                data.extend_from_slice(&response.bytes);
            }
            self.persist_market_download_chunk(MarketDownloadChunk {
                owner,
                config,
                partial_key: &partial_key,
                sha256,
                size,
                data: &data,
                etag: etag.as_deref(),
                object: &mut existing_id,
            })?;
        }
        if data.len() != size || package_sha256(&data) != sha256 {
            return Err(VmError::TypeError("downloaded Package SHA-256 is invalid"));
        }
        self.complete_market_download(CompletedMarketDownload {
            owner,
            config,
            partial_key,
            completed_key,
            sha256: sha256.to_owned(),
            size,
            data,
            etag,
            object: existing_id,
        })
    }

    fn persist_market_download_chunk(&self, chunk: MarketDownloadChunk<'_>) -> Result<(), VmError> {
        let MarketDownloadChunk {
            owner,
            config,
            partial_key,
            sha256,
            size,
            data,
            etag,
            object,
        } = chunk;
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let config_view = self.manager.read(system, config)?;
        let state = Value::Record(BTreeMap::from([
            ("owner".to_owned(), Value::Text(owner.to_string())),
            ("sha256".to_owned(), Value::Text(sha256.to_owned())),
            (
                "size".to_owned(),
                Value::Integer(
                    i64::try_from(size)
                        .map_err(|_| invalid_state("Market Package size is out of range"))?,
                ),
            ),
            ("data".to_owned(), Value::Bytes(data.to_vec())),
            (
                "etag".to_owned(),
                etag.map_or(Value::Null, |etag| Value::Text(etag.to_owned())),
            ),
            ("status".to_owned(), Value::Text("partial".to_owned())),
        ]));
        let mut transaction = self.manager.begin(system);
        if let Some(object_id) = *object {
            let view = self.manager.read(system, object_id)?;
            transaction
                .expect(object_id, view.header().version)
                .update_state(object_id, state.encode()?);
        } else {
            let id = ObjectId::new();
            let mut request = CreateObject::new(CORE_PACKAGE_DOWNLOAD_TYPE, state.encode()?)
                .with_id(id)
                .with_parent(config)
                .with_grant(owner, Capability::Inspect)
                .with_grant(owner, Capability::ViewValue)
                .with_grant(owner, Capability::Invoke);
            request.capabilities = [
                Capability::Inspect,
                Capability::ViewValue,
                Capability::Invoke,
            ]
            .into_iter()
            .collect();
            transaction
                .expect(config, config_view.header().version)
                .create(request)
                .set_link(config, partial_key, id);
            *object = Some(id);
        }
        self.manager.commit(transaction)?;
        Ok(())
    }

    fn complete_market_download(
        &self,
        download: CompletedMarketDownload,
    ) -> Result<ObjectId, VmError> {
        let CompletedMarketDownload {
            owner,
            config,
            partial_key,
            completed_key,
            sha256,
            size,
            data,
            etag,
            object,
        } = download;
        let Some(object) = object else {
            return Err(invalid_state(
                "Market download did not persist its first chunk",
            ));
        };
        let system = AccessContext::new(SYSTEM_SUBJECT);
        let config_view = self.manager.read(system, config)?;
        let object_view = self.manager.read(system, object)?;
        let state = Value::Record(BTreeMap::from([
            ("owner".to_owned(), Value::Text(owner.to_string())),
            ("sha256".to_owned(), Value::Text(sha256)),
            (
                "size".to_owned(),
                Value::Integer(
                    i64::try_from(size)
                        .map_err(|_| invalid_state("Market Package size is out of range"))?,
                ),
            ),
            ("data".to_owned(), Value::Bytes(data)),
            ("etag".to_owned(), etag.map_or(Value::Null, Value::Text)),
            ("status".to_owned(), Value::Text("complete".to_owned())),
        ]));
        let mut transaction = self.manager.begin(system);
        transaction
            .expect(config, config_view.header().version)
            .expect(object, object_view.header().version)
            .update_state(object, state.encode()?)
            .remove_link(config, partial_key)
            .set_link(config, completed_key, object);
        self.manager.commit(transaction)?;
        Ok(object)
    }

    // Market coordinates and SHA locks are checked recursively before import.
    #[expect(
        clippy::too_many_lines,
        reason = "market closure import is one recursive integrity workflow"
    )]
    fn install_market_closure(
        &self,
        owner: SubjectId,
        config: ObjectId,
        sha256: &str,
        depth: usize,
        visiting: &mut BTreeSet<String>,
        imported: &mut BTreeMap<String, ObjectId>,
    ) -> Result<ObjectId, VmError> {
        if depth > MAX_PACKAGE_DEPENDENCY_DEPTH || visiting.len() >= MAX_PACKAGE_DEPENDENCIES {
            return Err(VmError::TypeError(
                "Market dependency closure exceeds its limit",
            ));
        }
        if !valid_sha256(sha256) {
            return Err(VmError::TypeError(
                "dependency ID is not a lowercase SHA-256",
            ));
        }
        if let Some(artifact) = imported.get(sha256) {
            return Ok(*artifact);
        }
        let registry = self.package_registry_object()?;
        let registry_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), registry)?;
        if let Some(artifact) = registry_view.links().get(&format!("package:{sha256}")) {
            let artifact = *artifact;
            if !self.verify_package_artifact(artifact)? {
                return Err(invalid_state("registered Package is invalid"));
            }
            let index_configuration = self
                .manager
                .read(AccessContext::new(SYSTEM_SUBJECT), config)?;
            let Value::Record(index_configuration) = Value::decode(index_configuration.state())?
            else {
                return Err(invalid_state("Market configuration is malformed"));
            };
            let record = market_index_record(&index_configuration, sha256)?;
            let Value::Record(artifact_fields) = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), artifact)?
            else {
                return Err(invalid_state("Package is malformed"));
            };
            let Some(Value::Record(manifest)) = artifact_fields.get("manifest") else {
                return Err(invalid_state("Package Manifest is malformed"));
            };
            if record.get("coordinate") != Some(&Value::Text(package_coordinate(manifest)?)) {
                return Err(VmError::TypeError(
                    "registered Package coordinate does not match the Market index",
                ));
            }
            imported.insert(sha256.to_owned(), artifact);
            return Ok(artifact);
        }
        if !visiting.insert(sha256.to_owned()) {
            return Err(VmError::Provider(
                "Market Package dependency cycle detected".to_owned(),
            ));
        }
        let download = self.download_market_package(owner, config, sha256)?;
        let Value::Record(download_fields) = self
            .manager
            .value(AccessContext::new(SYSTEM_SUBJECT), download)?
        else {
            return Err(invalid_state("Market Package download is malformed"));
        };
        let Some(Value::Bytes(bytes)) = download_fields.get("data") else {
            return Err(invalid_state("Market Package download has no content"));
        };
        let Ok(Value::Record(manifest)) = Value::decode(bytes) else {
            return Err(VmError::TypeError(
                "download is not an Ousject Package; CilExec SQLite packages need a format converter",
            ));
        };
        let coordinate = package_coordinate(&manifest)?;
        let config_view = self
            .manager
            .read(AccessContext::new(SYSTEM_SUBJECT), config)?;
        let Value::Record(config_fields) = Value::decode(config_view.state())? else {
            return Err(invalid_state("Market configuration is malformed"));
        };
        let record = market_index_record(&config_fields, sha256)?;
        if record.get("coordinate") != Some(&Value::Text(coordinate.clone())) {
            return Err(VmError::TypeError(
                "downloaded Package coordinate does not match the Market index",
            ));
        }
        let Some(Value::Array(index_dependencies)) = record.get("dependencies") else {
            return Err(invalid_state("Market index dependencies are malformed"));
        };
        let mut required_market_dependencies = Vec::new();
        for dependency in index_dependencies {
            let Value::Record(fields) = dependency else {
                return Err(invalid_state("Market index dependency is malformed"));
            };
            let (Some(Value::Text(dependency_sha)), Some(Value::Bool(optional))) =
                (fields.get("sha256"), fields.get("optional"))
            else {
                return Err(invalid_state("Market index dependency is malformed"));
            };
            if !optional {
                required_market_dependencies.push(dependency_sha.clone());
            }
        }
        let package_dependencies = parse_package_dependencies(manifest.get("dependencies"))?;
        let mut manifest_dependency_hashes = package_dependencies
            .iter()
            .map(|dependency| dependency.sha256.clone())
            .collect::<Vec<_>>();
        required_market_dependencies.sort();
        manifest_dependency_hashes.sort();
        if required_market_dependencies != manifest_dependency_hashes {
            return Err(VmError::TypeError(
                "Market dependencies do not match the Ousject Package Manifest",
            ));
        }
        for dependency in package_dependencies {
            let dependency_artifact = self.install_market_closure(
                owner,
                config,
                &dependency.sha256,
                depth + 1,
                visiting,
                imported,
            )?;
            let Value::Record(dependency_value) = self
                .manager
                .value(AccessContext::new(SYSTEM_SUBJECT), dependency_artifact)?
            else {
                return Err(invalid_state("Market dependency Package is malformed"));
            };
            let Some(Value::Record(dependency_manifest)) = dependency_value.get("manifest") else {
                return Err(invalid_state("Market dependency Manifest is malformed"));
            };
            if package_coordinate(dependency_manifest)? != dependency.coordinate {
                return Err(VmError::TypeError(
                    "Market dependency coordinate does not match SHA",
                ));
            }
        }
        let artifact = self.import_package_artifact(bytes, Some(sha256))?;
        let Value::Record(artifact_fields) = self
            .manager
            .value(AccessContext::new(SYSTEM_SUBJECT), artifact)?
        else {
            return Err(invalid_state("imported Market Package is malformed"));
        };
        let Some(Value::Record(imported_manifest)) = artifact_fields.get("manifest") else {
            return Err(invalid_state("imported Market Manifest is malformed"));
        };
        if package_coordinate(imported_manifest)? != coordinate {
            return Err(VmError::TypeError(
                "Market Package coordinate changed during import",
            ));
        }
        visiting.remove(sha256);
        imported.insert(sha256.to_owned(), artifact);
        Ok(artifact)
    }
}

#[derive(Debug)]
struct MarketHttpResponse {
    status: u16,
    bytes: Vec<u8>,
    etag: Option<String>,
    content_range: Option<String>,
}

struct MarketDownloadChunk<'a> {
    owner: SubjectId,
    config: ObjectId,
    partial_key: &'a str,
    sha256: &'a str,
    size: usize,
    data: &'a [u8],
    etag: Option<&'a str>,
    object: &'a mut Option<ObjectId>,
}

struct CompletedMarketDownload {
    owner: SubjectId,
    config: ObjectId,
    partial_key: String,
    completed_key: String,
    sha256: String,
    size: usize,
    data: Vec<u8>,
    etag: Option<String>,
    object: Option<ObjectId>,
}

type MarketConfiguration = (ObjectId, ObjectView, BTreeMap<String, Value>);

fn market_http_get(url: &str, maximum: usize) -> Result<Vec<u8>, VmError> {
    let response = ureq::get(url)
        .timeout(std::time::Duration::from_secs(30))
        .call()
        .map_err(|error| VmError::Provider(format!("Market request failed: {error}")))?;
    let mut bytes = Vec::new();
    response
        .into_reader()
        .take(u64::try_from(maximum.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| VmError::Provider(format!("Market response read failed: {error}")))?;
    if bytes.len() > maximum {
        return Err(VmError::TypeError("Market response exceeds its size limit"));
    }
    Ok(bytes)
}

fn market_http_range(
    url: &str,
    start: usize,
    end: usize,
    etag: Option<&str>,
    maximum: usize,
) -> Result<MarketHttpResponse, VmError> {
    let range = format!("bytes={start}-{end}");
    let mut request = ureq::get(url)
        .set("Range", &range)
        .timeout(std::time::Duration::from_secs(30));
    if let Some(etag) = etag {
        request = request.set("If-Range", etag);
    }
    let response = request
        .call()
        .map_err(|error| VmError::Provider(format!("Market download failed: {error}")))?;
    let status = response.status();
    let etag = response.header("ETag").map(str::to_owned);
    let content_range = response.header("Content-Range").map(str::to_owned);
    let mut bytes = Vec::new();
    let maximum = maximum.min(MAX_PACKAGE_DOWNLOAD_OBJECT_BYTES);
    response
        .into_reader()
        .take(u64::try_from(maximum.saturating_add(1)).unwrap_or(u64::MAX))
        .read_to_end(&mut bytes)
        .map_err(|error| VmError::Provider(format!("Market chunk read failed: {error}")))?;
    if bytes.len() > maximum {
        return Err(VmError::TypeError("Market server sent an oversized chunk"));
    }
    Ok(MarketHttpResponse {
        status,
        bytes,
        etag,
        content_range,
    })
}

fn normalize_market_origin(origin: &str) -> Result<String, VmError> {
    let origin = origin.trim().trim_end_matches('/');
    let Some((scheme, authority)) = origin.split_once("://") else {
        return Err(VmError::TypeError("Market origin must use HTTP or HTTPS"));
    };
    if !matches!(scheme, "http" | "https")
        || authority.is_empty()
        || authority
            .chars()
            .any(|character| matches!(character, '/' | '?' | '#' | '@' | ' '))
        || origin.len() > 2048
    {
        return Err(VmError::TypeError("invalid Market origin"));
    }
    Ok(origin.to_owned())
}

fn market_user_link_key(owner: SubjectId) -> String {
    format!("user:{owner}")
}

fn market_index_record(
    configuration: &BTreeMap<String, Value>,
    sha256: &str,
) -> Result<BTreeMap<String, Value>, VmError> {
    let Some(Value::Array(records)) = configuration.get("index") else {
        return Err(VmError::Provider(
            "run market.update() before downloading".to_owned(),
        ));
    };
    records
        .iter()
        .find_map(|record| match record {
            Value::Record(fields)
                if fields.get("sha256") == Some(&Value::Text(sha256.to_owned())) =>
            {
                Some(fields.clone())
            }
            _ => None,
        })
        .ok_or_else(|| VmError::MissingKey(sha256.to_owned()))
}

fn market_record_matches(record: &Value, terms: &[String]) -> Result<bool, VmError> {
    let Value::Record(fields) = record else {
        return Err(invalid_state("Market index Package record is malformed"));
    };
    let mut searchable = Vec::new();
    for field in ["namespace", "name", "kind", "summary", "description"] {
        if let Some(Value::Text(value)) = fields.get(field) {
            searchable.extend(value.split_whitespace().map(str::to_lowercase));
        }
    }
    if let Some(Value::Text(value)) = fields.get("namespace") {
        searchable.push(value.to_lowercase());
    }
    if let Some(Value::Text(value)) = fields.get("name") {
        searchable.push(value.to_lowercase());
    }
    if let Some(Value::Text(value)) = fields.get("coordinate") {
        searchable.push(value.to_lowercase());
    }
    if let Some(Value::Array(tags)) = fields.get("tags") {
        for tag in tags {
            if let Value::Text(tag) = tag {
                searchable.push(tag.to_lowercase());
            }
        }
    }
    let hash = fields.get("sha256").and_then(|value| match value {
        Value::Text(hash) => Some(hash.as_str()),
        _ => None,
    });
    Ok(terms.iter().all(|term| {
        searchable.iter().any(|field| field.starts_with(term))
            || term.len() >= 8 && hash.is_some_and(|hash| hash.starts_with(term))
    }))
}
