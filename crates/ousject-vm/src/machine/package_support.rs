#![allow(clippy::wildcard_imports)]

use super::*;

pub(super) const MAX_PACKAGE_MODULE_BYTES: usize = 1024 * 1024;
pub(super) const MAX_PACKAGE_SOURCE_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_PACKAGE_EXPANDED_SOURCE_BYTES: usize = 64 * 1024 * 1024;
pub(super) const MAX_PACKAGE_COMPILED_BYTES: usize = 32 * 1024 * 1024;
pub(super) const MAX_PACKAGE_DEPENDENCIES: usize = 256;
pub(super) const MAX_PACKAGE_DEPENDENCY_DEPTH: usize = 64;
pub(super) const MAX_PACKAGE_DATA_BYTES: usize = 8 * 1024 * 1024;
pub(super) const MAX_PACKAGE_CAPABILITIES: usize = 128;

#[derive(Clone, Debug)]
pub(super) struct PackageDependency {
    pub coordinate: String,
    pub sha256: String,
}

pub(super) struct ResolvedPackage {
    pub package: ObjectId,
    pub coordinate: String,
    pub sha256: String,
    pub kind: Value,
    pub modules: BTreeMap<String, Value>,
    pub dependencies: Vec<PackageDependency>,
}

pub(super) fn parse_package_dependencies(
    value: Option<&Value>,
) -> Result<Vec<PackageDependency>, VmError> {
    let Some(Value::Array(values)) = value else {
        return Err(invalid_state("Package Manifest dependencies are malformed"));
    };
    if values.len() > MAX_PACKAGE_DEPENDENCIES {
        return Err(VmError::TypeError("Package has more than 256 dependencies"));
    }
    let mut dependencies = Vec::with_capacity(values.len());
    let mut coordinates = BTreeSet::new();
    for value in values {
        let Value::Record(fields) = value else {
            return Err(invalid_state("Package dependency is malformed"));
        };
        let (Some(Value::Text(coordinate)), Some(Value::Text(sha256))) =
            (fields.get("coordinate"), fields.get("sha256"))
        else {
            return Err(invalid_state(
                "Package dependency is missing its coordinate or SHA-256",
            ));
        };
        validate_full_coordinate(coordinate)?;
        if !valid_sha256(sha256) || !coordinates.insert(coordinate.clone()) {
            return Err(invalid_state(
                "Package dependency SHA-256 or coordinate is invalid",
            ));
        }
        if fields
            .keys()
            .any(|field| !matches!(field.as_str(), "coordinate" | "sha256"))
        {
            return Err(invalid_state("Package dependency has an unknown field"));
        }
        dependencies.push(PackageDependency {
            coordinate: coordinate.clone(),
            sha256: sha256.clone(),
        });
    }
    dependencies.sort_by(|left, right| left.coordinate.cmp(&right.coordinate));
    Ok(dependencies)
}

pub(super) fn package_dependencies_value(dependencies: &[PackageDependency]) -> Value {
    Value::Array(
        dependencies
            .iter()
            .map(|dependency| {
                Value::Record(BTreeMap::from([
                    (
                        "coordinate".to_owned(),
                        Value::Text(dependency.coordinate.clone()),
                    ),
                    ("sha256".to_owned(), Value::Text(dependency.sha256.clone())),
                ]))
            })
            .collect(),
    )
}

pub(super) fn valid_sha256(hash: &str) -> bool {
    hash.len() == 64
        && hash
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
}

pub(super) fn value_record<'a>(
    value: &'a Value,
    description: &str,
) -> Result<&'a BTreeMap<String, Value>, VmError> {
    match value {
        Value::Record(fields) | Value::Map(fields) => Ok(fields),
        _ => Err(VmError::TypeError(match description {
            "Package specification" => "Package specification must be a Record",
            _ => "expected a Record",
        })),
    }
}

pub(super) fn required_text<'a>(
    fields: &'a BTreeMap<String, Value>,
    field: &str,
) -> Result<&'a str, VmError> {
    match fields.get(field) {
        Some(Value::Text(value)) => Ok(value),
        _ => Err(VmError::TypeError(
            "Package specification has a missing Text field",
        )),
    }
}

pub(super) fn normalize_capabilities(value: Option<&Value>) -> Result<Vec<String>, VmError> {
    let values = match value {
        Some(Value::Array(values)) => values,
        None | Some(Value::Null) => return Ok(Vec::new()),
        _ => return Err(VmError::TypeError("Package capabilities must be an Array")),
    };
    if values.len() > MAX_PACKAGE_CAPABILITIES {
        return Err(VmError::TypeError(
            "Package may declare at most 128 capabilities",
        ));
    }
    let mut capabilities = BTreeSet::new();
    for value in values {
        let Value::Text(capability) = value else {
            return Err(VmError::TypeError("Package capabilities must be Text"));
        };
        if capability.is_empty()
            || capability.len() > 128
            || !capabilities.insert(capability.clone())
        {
            return Err(VmError::TypeError(
                "Package capability is empty, too long, or duplicated",
            ));
        }
    }
    Ok(capabilities.into_iter().collect())
}

pub(super) fn normalize_package_exports(value: Option<&Value>) -> Result<Value, VmError> {
    let values = match value {
        Some(Value::Record(values) | Value::Map(values)) => values,
        None | Some(Value::Null) => return Ok(Value::Record(BTreeMap::new())),
        _ => return Err(VmError::TypeError("Package exports must be a Map")),
    };
    if values.len() > 256 {
        return Err(VmError::TypeError("Package has more than 256 exports"));
    }
    let mut exports = BTreeMap::new();
    for (name, value) in values {
        if !valid_package_export_name(name)
            || matches!(
                name.as_str(),
                "info"
                    | "verify"
                    | "module"
                    | "resource"
                    | "permissions"
                    | "data"
                    | "run"
                    | "upgrade"
                    | "rollback"
                    | "uninstall"
            )
        {
            return Err(VmError::TypeError("invalid Package export name"));
        }
        let (Value::Record(descriptor) | Value::Map(descriptor)) = value else {
            return Err(VmError::TypeError("Package export must be a Record"));
        };
        let (Some(Value::Text(module)), Some(Value::Text(function))) =
            (descriptor.get("module"), descriptor.get("function"))
        else {
            return Err(VmError::TypeError(
                "Package export requires Text module and function fields",
            ));
        };
        if !valid_package_module_name(module) || !valid_package_export_name(function) {
            return Err(VmError::TypeError("invalid Package export target"));
        }
        if !matches!(descriptor.get("arguments"), Some(Value::Integer(count)) if (0..=64).contains(count))
        {
            return Err(VmError::TypeError(
                "Package export requires an Integer arguments field from 0 to 64",
            ));
        }
        if descriptor
            .keys()
            .any(|key| !matches!(key.as_str(), "module" | "function" | "arguments"))
        {
            return Err(VmError::TypeError("unknown Package export field"));
        }
        exports.insert(name.clone(), Value::Record(descriptor.clone()));
    }
    Ok(Value::Record(exports))
}

pub(super) fn valid_package_export_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes
        .next()
        .is_some_and(|byte| byte.is_ascii_alphabetic() || byte == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        && name.len() <= 128
}

pub(super) fn validate_coordinate(component: &str, _field: &str) -> Result<(), VmError> {
    if component.is_empty()
        || component.len() > 128
        || !component.as_bytes()[0].is_ascii_alphanumeric()
        || !component
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        return Err(VmError::TypeError("invalid Package namespace or name"));
    }
    Ok(())
}

pub(super) fn validate_release(release: &str) -> Result<(), VmError> {
    if release.is_empty()
        || release.len() > 64
        || !release.as_bytes()[0].is_ascii_alphanumeric()
        || !release
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'+' | b'-'))
    {
        return Err(VmError::TypeError("invalid Package release"));
    }
    Ok(())
}

pub(super) fn valid_package_module_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name.as_bytes()[0].is_ascii_alphanumeric()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b'/'))
        && !name
            .split('/')
            .any(|part| part.is_empty() || matches!(part, "." | ".."))
}

pub(super) fn artifact_manifest_hash(value: &Value) -> Result<String, VmError> {
    let Value::Record(fields) = value else {
        return Err(invalid_state("Package is malformed"));
    };
    let manifest = fields
        .get("manifest")
        .ok_or_else(|| invalid_state("Package has no Manifest"))?;
    Ok(package_sha256(&manifest.encode()?))
}

pub(super) fn package_coordinate(manifest: &BTreeMap<String, Value>) -> Result<String, VmError> {
    let namespace = required_text(manifest, "namespace")?;
    let name = required_text(manifest, "name")?;
    let version = required_text(manifest, "version")?;
    Ok(format!("{namespace}/{name}/{version}"))
}

pub(super) fn validate_full_coordinate(coordinate: &str) -> Result<(), VmError> {
    let parts = coordinate.split('/').collect::<Vec<_>>();
    if parts.len() != 3 {
        return Err(VmError::TypeError(
            "Package coordinate must be namespace/name/version",
        ));
    }
    validate_coordinate(parts[0], "namespace")?;
    validate_coordinate(parts[1], "name")?;
    validate_release(parts[2])
}

pub(super) fn package_user_index_key(owner: SubjectId) -> String {
    format!("user:{owner}")
}

pub(super) fn package_user_link_key(coordinate: &str) -> String {
    encoded_link_key("package:", coordinate)
}

pub(super) fn package_default_link_key(package_name: &str) -> String {
    encoded_link_key("default:", package_name)
}

pub(super) fn package_restore_link_key(retired_installation: ObjectId) -> String {
    encoded_link_key("restore:", &retired_installation.to_string())
}

pub(super) fn package_module_link_key(name: &str) -> String {
    encoded_link_key("module:", name)
}

pub(super) fn package_dependency_link_key(coordinate: &str) -> String {
    encoded_link_key("dependency:", coordinate)
}

pub(super) fn package_required_by_link_key(installation: ObjectId) -> String {
    format!("required_by:{installation}")
}

fn encoded_link_key(prefix: &str, value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut key = String::with_capacity(prefix.len() + value.len() * 2);
    key.push_str(prefix);
    for byte in value.bytes() {
        key.push(char::from(HEX[usize::from(byte >> 4)]));
        key.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    key
}

pub(super) fn package_module_import_name(coordinate: &str, module: &str) -> String {
    format!("package/{coordinate}/{module}")
}

pub(super) fn package_user_index_matches(value: &Value, owner: SubjectId) -> bool {
    matches!(value, Value::Record(fields)
        if fields.get("owner") == Some(&Value::Text(owner.to_string())))
}

pub(super) fn package_installation_owned(value: &Value, owner: SubjectId) -> bool {
    matches!(value, Value::Record(fields)
        if fields.get("owner") == Some(&Value::Text(owner.to_string()))
            && fields.get("status") == Some(&Value::Text("installed".to_owned())))
}

pub(super) fn package_installation_matches(
    value: &Value,
    owner: SubjectId,
    package: ObjectId,
    hash: &str,
) -> bool {
    matches!(value, Value::Record(fields)
        if package_installation_owned(value, owner)
            && fields.get("package") == Some(&Value::Text(package.to_string()))
            && fields.get("sha256") == Some(&Value::Text(hash.to_owned())))
}

pub(super) fn package_modules_summary(value: Option<&Value>) -> Result<Value, VmError> {
    let Some(Value::Record(modules)) = value else {
        return Err(invalid_state("Package Manifest modules are malformed"));
    };
    modules
        .iter()
        .map(|(name, module)| Ok((name.clone(), package_component_summary(Some(module))?)))
        .collect::<Result<BTreeMap<_, _>, VmError>>()
        .map(Value::Record)
}

pub(super) fn package_resources_summary(value: Option<&Value>) -> Result<Value, VmError> {
    let resources = match value {
        Some(Value::Record(resources)) => resources,
        None => return Ok(Value::Record(BTreeMap::new())),
        _ => return Err(invalid_state("Package Manifest resources are malformed")),
    };
    resources
        .iter()
        .map(|(name, value)| {
            let encoded = value.encode()?;
            let size = i64::try_from(encoded.len())
                .map_err(|_| VmError::TypeError("Package resource size exceeds integer range"))?;
            Ok((
                name.clone(),
                Value::Record(BTreeMap::from([
                    ("sha256".to_owned(), Value::Text(package_sha256(&encoded))),
                    ("bytes".to_owned(), Value::Integer(size)),
                ])),
            ))
        })
        .collect::<Result<BTreeMap<_, _>, VmError>>()
        .map(Value::Record)
}

pub(super) fn package_component_summary(value: Option<&Value>) -> Result<Value, VmError> {
    let Some(value) = value else {
        return Err(invalid_state("Package component is missing"));
    };
    if matches!(value, Value::Null) {
        return Ok(Value::Null);
    }
    let Value::Record(fields) = value else {
        return Err(invalid_state("Package component is malformed"));
    };
    let source = match fields.get("source") {
        Some(Value::Text(source)) => source,
        _ => return Err(invalid_state("Package component has no source")),
    };
    let program = match fields.get("program") {
        Some(Value::Bytes(program)) => program,
        _ => return Err(invalid_state("Package component has no Program")),
    };
    let byte_count = |length: usize| {
        i64::try_from(length)
            .map(Value::Integer)
            .map_err(|_| VmError::TypeError("Package component size exceeds the integer range"))
    };
    Ok(Value::Record(BTreeMap::from([
        (
            "source_sha256".to_owned(),
            fields
                .get("source_sha256")
                .cloned()
                .ok_or_else(|| invalid_state("Package component has no source hash"))?,
        ),
        ("source_bytes".to_owned(), byte_count(source.len())?),
        ("program_bytes".to_owned(), byte_count(program.len())?),
    ])))
}

pub(super) fn package_sha256(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut output = String::with_capacity(64);
    for byte in ousject_auth::sha256_digest(bytes) {
        output.push(char::from(HEX[usize::from(byte >> 4)]));
        output.push(char::from(HEX[usize::from(byte & 0x0f)]));
    }
    output
}
