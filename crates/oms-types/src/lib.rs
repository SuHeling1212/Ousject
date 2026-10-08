//! Shared value types for the Ousject Object Management System.

#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

use alloc::borrow::ToOwned;
use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt;
use core::str::FromStr;
#[cfg(not(feature = "std"))]
use core::sync::atomic::AtomicU8;
use core::sync::atomic::{AtomicU64, Ordering};

#[cfg(feature = "std")]
use std::sync::OnceLock;
#[cfg(feature = "std")]
use std::time::{SystemTime, UNIX_EPOCH};

static NEXT_ID: AtomicU64 = AtomicU64::new(1);
#[cfg(feature = "std")]
static BOOT_ID_PREFIX: OnceLock<u64> = OnceLock::new();
#[cfg(not(feature = "std"))]
static ID_PREFIX: AtomicU64 = AtomicU64::new(0);
// 0 = uninitialized, 1 = initialization in progress, 2 = ready.
#[cfg(not(feature = "std"))]
static ID_GENERATOR_STATE: AtomicU8 = AtomicU8::new(0);

/// Error returned when the Native ID generator was already initialized.
#[cfg(not(feature = "std"))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdGeneratorAlreadyInitialized;

#[cfg(not(feature = "std"))]
impl fmt::Display for IdGeneratorAlreadyInitialized {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("identifier generator was already initialized")
    }
}

/// Seeds the Native ID generator with a boot-unique 64-bit prefix.
///
/// Native startup must call this once, using platform entropy, before any
/// `ObjectId::new`, `TypeId::new`, `TransactionId::new`, or `SubjectId::new`.
/// Refusing to generate IDs before seeding avoids silently reusing the same
/// identifier range after a machine restart.
///
/// # Errors
///
/// Returns [`IdGeneratorAlreadyInitialized`] if another caller has already
/// started initialization.
#[cfg(not(feature = "std"))]
pub fn seed_id_generator(prefix: u64) -> Result<(), IdGeneratorAlreadyInitialized> {
    ID_GENERATOR_STATE
        .compare_exchange(0, 1, Ordering::Acquire, Ordering::Acquire)
        .map_err(|_| IdGeneratorAlreadyInitialized)?;
    ID_PREFIX.store(prefix, Ordering::Relaxed);
    ID_GENERATOR_STATE.store(2, Ordering::Release);
    Ok(())
}

fn id_prefix() -> u64 {
    #[cfg(feature = "std")]
    {
        *BOOT_ID_PREFIX.get_or_init(|| {
            let timestamp = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_nanos());
            let bytes = timestamp.to_le_bytes();
            u64::from_le_bytes([
                bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
            ])
        })
    }
    #[cfg(all(not(feature = "std"), not(test)))]
    {
        assert_eq!(
            ID_GENERATOR_STATE.load(Ordering::Acquire),
            2,
            "Native identifier generator must be seeded before allocating IDs"
        );
        ID_PREFIX.load(Ordering::Relaxed)
    }
    #[cfg(all(not(feature = "std"), test))]
    {
        1
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ParseIdError;

impl fmt::Display for ParseIdError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("identifier must contain 1 to 32 hexadecimal digits")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ParseIdError {}

macro_rules! id_type {
    ($name:ident) => {
        #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub struct $name(u128);

        impl $name {
            #[must_use]
            pub const fn from_u128(value: u128) -> Self {
                Self(value)
            }

            #[must_use]
            pub const fn as_u128(self) -> u128 {
                self.0
            }

            #[must_use]
            pub fn new() -> Self {
                let sequence = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                let prefix = id_prefix();
                Self((u128::from(prefix) << 64) | u128::from(sequence))
            }
        }

        impl Default for $name {
            fn default() -> Self {
                Self::new()
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{}({:032x})", stringify!($name), self.0)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(formatter, "{:032x}", self.0)
            }
        }

        impl FromStr for $name {
            type Err = ParseIdError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                let value = value.strip_prefix("0x").unwrap_or(value);
                if value.is_empty() || value.len() > 32 {
                    return Err(ParseIdError);
                }
                u128::from_str_radix(value, 16)
                    .map(Self)
                    .map_err(|_| ParseIdError)
            }
        }
    };
}

id_type!(ObjectId);
id_type!(TypeId);
id_type!(TransactionId);
id_type!(SubjectId);

/// Stable public name of the highest local system identity.
pub const LOCAL_USER_NAME: &str = "local";
/// Trusted local kernel/user identity. Ordinary User Processes carry a
/// different persistent `SubjectId`.
pub const LOCAL_SUBJECT: SubjectId = SubjectId::from_u128(1);
/// Internal compatibility name for code executing on behalf of `local`.
pub const SYSTEM_SUBJECT: SubjectId = LOCAL_SUBJECT;

pub const TYPE_DESCRIPTOR_TYPE: TypeId = TypeId::from_u128(0x0100);
pub const CORE_VALUE_TYPE: TypeId = TypeId::from_u128(0x1000);
pub const CORE_TEXT_TYPE: TypeId = TypeId::from_u128(0x1001);
pub const CORE_BYTES_TYPE: TypeId = TypeId::from_u128(0x1002);
pub const CORE_COLLECTION_TYPE: TypeId = TypeId::from_u128(0x1003);
pub const CORE_NAMESPACE_TYPE: TypeId = TypeId::from_u128(0x1004);
pub const CORE_INSTANCE_TYPE: TypeId = TypeId::from_u128(0x1005);
pub const CORE_PROGRAM_TYPE: TypeId = TypeId::from_u128(0x1100);
pub const CORE_PROCESS_TYPE: TypeId = TypeId::from_u128(0x1101);
pub const CORE_USER_TYPE: TypeId = TypeId::from_u128(0x1102);
pub const CORE_SESSION_TYPE: TypeId = TypeId::from_u128(0x1104);
pub const CORE_EFFECT_TYPE: TypeId = TypeId::from_u128(0x1105);
pub const CORE_CHANNEL_TYPE: TypeId = TypeId::from_u128(0x1106);
pub const CORE_SYSTEM_TYPE: TypeId = TypeId::from_u128(0x1107);
pub const CORE_AUTHENTICATION_TYPE: TypeId = TypeId::from_u128(0x1108);
pub const CORE_USER_REGISTRY_TYPE: TypeId = TypeId::from_u128(0x1109);
pub const CORE_SCHEDULER_TYPE: TypeId = TypeId::from_u128(0x110a);
pub const CORE_COMPILER_TYPE: TypeId = TypeId::from_u128(0x110b);
pub const CORE_TYPE_REGISTRY_TYPE: TypeId = TypeId::from_u128(0x110c);
pub const CORE_PROVIDER_REGISTRY_TYPE: TypeId = TypeId::from_u128(0x110d);
pub const CORE_OBJECT_STORE_TYPE: TypeId = TypeId::from_u128(0x110e);
pub const CORE_MATH_TYPE: TypeId = TypeId::from_u128(0x110f);
pub const CORE_TIME_TYPE: TypeId = TypeId::from_u128(0x1110);
pub const CORE_TERMINAL_TYPE: TypeId = TypeId::from_u128(0x1111);
pub const CORE_MODULE_TYPE: TypeId = TypeId::from_u128(0x1113);
pub const CORE_MODULE_REGISTRY_TYPE: TypeId = TypeId::from_u128(0x1114);
pub const CORE_MODULE_INSTANCE_TYPE: TypeId = TypeId::from_u128(0x1115);
pub const CORE_PACKAGE_TYPE: TypeId = TypeId::from_u128(0x1116);
pub const CORE_PACKAGE_REGISTRY_TYPE: TypeId = TypeId::from_u128(0x1117);
pub const CORE_PACKAGE_INSTALLATION_TYPE: TypeId = TypeId::from_u128(0x1118);
pub const CORE_PACKAGE_DATA_TYPE: TypeId = TypeId::from_u128(0x1119);
pub const CORE_PACKAGE_MODULE_TYPE: TypeId = TypeId::from_u128(0x111a);
pub const CORE_CRYPTO_TYPE: TypeId = TypeId::from_u128(0x111b);
pub const CORE_PACKAGE_SUBJECT_TYPE: TypeId = TypeId::from_u128(0x111c);
pub const CORE_PACKAGE_INSTANCE_TYPE: TypeId = TypeId::from_u128(0x111d);
pub const CORE_PACKAGE_AUDIT_TYPE: TypeId = TypeId::from_u128(0x111e);
pub const CORE_PACKAGE_MARKET_TYPE: TypeId = TypeId::from_u128(0x111f);
pub const CORE_PACKAGE_MARKET_CONFIG_TYPE: TypeId = TypeId::from_u128(0x1120);
pub const CORE_PACKAGE_DOWNLOAD_TYPE: TypeId = TypeId::from_u128(0x1121);
pub const CORE_SWAP_POOL_TYPE: TypeId = TypeId::from_u128(0x1122);
pub const CORE_TIMER_TYPE: TypeId = TypeId::from_u128(0x1123);
pub const CORE_AUDIT_TYPE: TypeId = TypeId::from_u128(0x1124);
pub const CORE_AUDIT_EVENT_TYPE: TypeId = TypeId::from_u128(0x1125);
pub const NET_ENDPOINT_TYPE: TypeId = TypeId::from_u128(0x1200);
pub const NET_RESOLVER_TYPE: TypeId = TypeId::from_u128(0x1201);
pub const DEVICE_DISPLAY_TYPE: TypeId = TypeId::from_u128(0x1300);
pub const DEVICE_SENSOR_TYPE: TypeId = TypeId::from_u128(0x1301);
pub const DEVICE_KEYBOARD_TYPE: TypeId = TypeId::from_u128(0x1302);
pub const DEVICE_BLOCK_STORAGE_TYPE: TypeId = TypeId::from_u128(0x1303);

const VALUE_MAGIC: &[u8; 4] = b"OVL0";
const MAX_VALUE_DEPTH: usize = 64;
const MAX_VALUE_ITEMS: usize = 1_000_000;
const MAX_VALUE_BYTES: usize = 16 * 1024 * 1024;

/// An equality-safe IEEE-754 value. The exact bit pattern is preserved so
/// serialized Values have deterministic equality, including NaN payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct FloatValue(u64);

impl FloatValue {
    #[must_use]
    pub const fn from_bits(bits: u64) -> Self {
        Self(bits)
    }

    #[must_use]
    pub fn new(value: f64) -> Self {
        Self(value.to_bits())
    }

    #[must_use]
    pub const fn bits(self) -> u64 {
        self.0
    }

    #[must_use]
    pub fn get(self) -> f64 {
        f64::from_bits(self.0)
    }
}

impl From<f64> for FloatValue {
    fn from(value: f64) -> Self {
        Self::new(value)
    }
}

/// The common inline value representation used by Object state and TF.
/// Object identities are exchanged as plain text, not reference Values.
/// Managed relationships remain explicit parent/child edges or named links.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Value {
    Null,
    Bool(bool),
    Integer(i64),
    Float(FloatValue),
    Text(String),
    Bytes(Vec<u8>),
    Array(Vec<Self>),
    Map(BTreeMap<String, Self>),
    Record(BTreeMap<String, Self>),
    Error { code: String, message: String },
}

impl Value {
    #[must_use]
    pub fn is_truthy(&self) -> bool {
        match self {
            Self::Null => false,
            Self::Bool(value) => *value,
            Self::Integer(value) => *value != 0,
            Self::Float(value) => value.get() != 0.0,
            Self::Text(value) => !value.is_empty(),
            Self::Bytes(value) => !value.is_empty(),
            Self::Array(value) => !value.is_empty(),
            Self::Map(value) | Self::Record(value) => !value.is_empty(),
            Self::Error { .. } => true,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Null => "null",
            Self::Bool(_) => "bool",
            Self::Integer(_) => "integer",
            Self::Float(_) => "float",
            Self::Text(_) => "text",
            Self::Bytes(_) => "bytes",
            Self::Array(_) => "array",
            Self::Map(_) => "map",
            Self::Record(_) => "record",
            Self::Error { .. } => "error",
        }
    }

    /// Encodes a Value into the pre-release OVL0 representation.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError::LimitExceeded`] when nesting or data size limits
    /// are exceeded.
    pub fn encode(&self) -> Result<Vec<u8>, ValueError> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(VALUE_MAGIC);
        encode_value(&mut bytes, self, 0)?;
        if bytes.len() > MAX_VALUE_BYTES {
            return Err(ValueError::LimitExceeded);
        }
        Ok(bytes)
    }

    /// Decodes and validates an OVL0 Value.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, oversized or unknown data.
    pub fn decode(bytes: &[u8]) -> Result<Self, ValueError> {
        if bytes.len() > MAX_VALUE_BYTES {
            return Err(ValueError::LimitExceeded);
        }
        let mut reader = ValueReader::new(bytes);
        if reader.take(4)? != VALUE_MAGIC {
            return Err(ValueError::InvalidMagic);
        }
        let value = decode_value(&mut reader, 0)?;
        if !reader.is_empty() {
            return Err(ValueError::TrailingData);
        }
        Ok(value)
    }
}

impl fmt::Display for Value {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => formatter.write_str("null"),
            Self::Bool(value) => write!(formatter, "{value}"),
            Self::Integer(value) => write!(formatter, "{value}"),
            Self::Float(value) => write!(formatter, "{}", value.get()),
            Self::Text(value) => formatter.write_str(value),
            Self::Bytes(value) => write!(formatter, "<{} bytes>", value.len()),
            Self::Array(value) => write!(formatter, "{value:?}"),
            Self::Map(value) | Self::Record(value) => write!(formatter, "{value:?}"),
            Self::Error { code, message } => write!(formatter, "{code}: {message}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValueError {
    InvalidMagic,
    UnexpectedEnd,
    InvalidTag(u8),
    InvalidUtf8,
    LimitExceeded,
    DuplicateKey(String),
    TrailingData,
}

impl fmt::Display for ValueError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for ValueError {}

fn encode_value(bytes: &mut Vec<u8>, value: &Value, depth: usize) -> Result<(), ValueError> {
    if depth > MAX_VALUE_DEPTH || bytes.len() > MAX_VALUE_BYTES {
        return Err(ValueError::LimitExceeded);
    }
    match value {
        Value::Null => bytes.push(0),
        Value::Bool(false) => bytes.push(1),
        Value::Bool(true) => bytes.push(2),
        Value::Integer(value) => {
            bytes.push(3);
            bytes.extend_from_slice(&value.to_le_bytes());
        }
        Value::Float(value) => {
            bytes.push(4);
            bytes.extend_from_slice(&value.bits().to_le_bytes());
        }
        Value::Text(value) => {
            bytes.push(5);
            encode_bytes(bytes, value.as_bytes())?;
        }
        Value::Bytes(value) => {
            bytes.push(6);
            encode_bytes(bytes, value)?;
        }
        Value::Array(values) => {
            bytes.push(7);
            encode_len(bytes, values.len())?;
            for value in values {
                encode_value(bytes, value, depth + 1)?;
            }
        }
        Value::Map(values) => {
            bytes.push(8);
            encode_entries(bytes, values, depth)?;
        }
        Value::Record(values) => {
            bytes.push(9);
            encode_entries(bytes, values, depth)?;
        }
        Value::Error { code, message } => {
            bytes.push(10);
            encode_bytes(bytes, code.as_bytes())?;
            encode_bytes(bytes, message.as_bytes())?;
        }
    }
    if bytes.len() > MAX_VALUE_BYTES {
        return Err(ValueError::LimitExceeded);
    }
    Ok(())
}

fn encode_entries(
    bytes: &mut Vec<u8>,
    values: &BTreeMap<String, Value>,
    depth: usize,
) -> Result<(), ValueError> {
    encode_len(bytes, values.len())?;
    for (key, value) in values {
        encode_bytes(bytes, key.as_bytes())?;
        encode_value(bytes, value, depth + 1)?;
    }
    Ok(())
}

fn encode_bytes(bytes: &mut Vec<u8>, value: &[u8]) -> Result<(), ValueError> {
    encode_len(bytes, value.len())?;
    bytes.extend_from_slice(value);
    Ok(())
}

fn encode_len(bytes: &mut Vec<u8>, value: usize) -> Result<(), ValueError> {
    if value > MAX_VALUE_ITEMS || value > MAX_VALUE_BYTES {
        return Err(ValueError::LimitExceeded);
    }
    let value = u32::try_from(value).map_err(|_| ValueError::LimitExceeded)?;
    bytes.extend_from_slice(&value.to_le_bytes());
    Ok(())
}

fn decode_value(reader: &mut ValueReader<'_>, depth: usize) -> Result<Value, ValueError> {
    if depth > MAX_VALUE_DEPTH {
        return Err(ValueError::LimitExceeded);
    }
    Ok(match reader.u8()? {
        0 => Value::Null,
        1 => Value::Bool(false),
        2 => Value::Bool(true),
        3 => Value::Integer(reader.i64()?),
        4 => Value::Float(FloatValue::from_bits(reader.u64()?)),
        5 => Value::Text(reader.string()?),
        6 => Value::Bytes(reader.bytes()?.to_vec()),
        7 => {
            let count = reader.count()?;
            let mut values = Vec::with_capacity(count);
            for _ in 0..count {
                values.push(decode_value(reader, depth + 1)?);
            }
            Value::Array(values)
        }
        8 => Value::Map(decode_entries(reader, depth)?),
        9 => Value::Record(decode_entries(reader, depth)?),
        10 => Value::Error {
            code: reader.string()?,
            message: reader.string()?,
        },
        tag => return Err(ValueError::InvalidTag(tag)),
    })
}

fn decode_entries(
    reader: &mut ValueReader<'_>,
    depth: usize,
) -> Result<BTreeMap<String, Value>, ValueError> {
    let count = reader.count()?;
    let mut values = BTreeMap::new();
    for _ in 0..count {
        let key = reader.string()?;
        let value = decode_value(reader, depth + 1)?;
        if values.insert(key.clone(), value).is_some() {
            return Err(ValueError::DuplicateKey(key));
        }
    }
    Ok(values)
}

struct ValueReader<'a> {
    bytes: &'a [u8],
    position: usize,
}

impl<'a> ValueReader<'a> {
    const fn new(bytes: &'a [u8]) -> Self {
        Self { bytes, position: 0 }
    }

    fn take(&mut self, count: usize) -> Result<&'a [u8], ValueError> {
        let end = self
            .position
            .checked_add(count)
            .ok_or(ValueError::LimitExceeded)?;
        let value = self
            .bytes
            .get(self.position..end)
            .ok_or(ValueError::UnexpectedEnd)?;
        self.position = end;
        Ok(value)
    }

    fn u8(&mut self) -> Result<u8, ValueError> {
        Ok(self.take(1)?[0])
    }

    fn u32(&mut self) -> Result<u32, ValueError> {
        let mut bytes = [0; 4];
        bytes.copy_from_slice(self.take(4)?);
        Ok(u32::from_le_bytes(bytes))
    }

    fn u64(&mut self) -> Result<u64, ValueError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(u64::from_le_bytes(bytes))
    }

    fn i64(&mut self) -> Result<i64, ValueError> {
        let mut bytes = [0; 8];
        bytes.copy_from_slice(self.take(8)?);
        Ok(i64::from_le_bytes(bytes))
    }

    fn count(&mut self) -> Result<usize, ValueError> {
        let count = usize::try_from(self.u32()?).map_err(|_| ValueError::LimitExceeded)?;
        if count > MAX_VALUE_ITEMS {
            return Err(ValueError::LimitExceeded);
        }
        Ok(count)
    }

    fn bytes(&mut self) -> Result<&'a [u8], ValueError> {
        let count = self.count()?;
        self.take(count)
    }

    fn string(&mut self) -> Result<String, ValueError> {
        core::str::from_utf8(self.bytes()?)
            .map(str::to_owned)
            .map_err(|_| ValueError::InvalidUtf8)
    }

    const fn is_empty(&self) -> bool {
        self.position == self.bytes.len()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ObjectVersion(u64);

impl ObjectVersion {
    #[must_use]
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u64 {
        self.0
    }

    #[must_use]
    pub const fn next(self) -> Self {
        Self(self.0 + 1)
    }

    #[must_use]
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(value) => Some(Self(value)),
            None => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ShardId(u32);

impl ShardId {
    #[must_use]
    pub const fn new(value: u32) -> Self {
        Self(value)
    }

    #[must_use]
    pub const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LifecycleState {
    Creating,
    Active,
    Suspended,
    Migrating,
    Terminating,
    Tombstoned,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Capability {
    ViewValue,
    ReplaceValue,
    CreateChild,
    Invoke,
    Link,
    Reparent,
    Retire,
    Inspect,
    ManagePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectHeader {
    pub id: ObjectId,
    pub type_id: TypeId,
    pub parent_id: Option<ObjectId>,
    pub version: ObjectVersion,
    pub lifecycle: LifecycleState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OmsError {
    NotFound(ObjectId),
    Conflict {
        object: ObjectId,
        expected: ObjectVersion,
        actual: ObjectVersion,
    },
    Denied {
        object: ObjectId,
        capability: Capability,
    },
    InvalidLifecycle {
        object: ObjectId,
        state: LifecycleState,
    },
    CrossShardTransaction,
    ParentCycle,
    InvalidShardCount,
    InvalidOperation(&'static str),
    UnknownTypeName(String),
    UnknownType(TypeId),
    TypeCreationDenied(TypeId),
    ValueSchemaMismatch {
        type_id: TypeId,
        expected: &'static str,
        actual: &'static str,
    },
    InvalidValue(String),
    InvalidName(String),
    NameNotFound {
        namespace: ObjectId,
        name: String,
    },
    TemporarilyUnavailable,
    StoreInUse(String),
    VersionExhausted(ObjectId),
    Storage(String),
    Corruption(String),
}

impl fmt::Display for OmsError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{self:?}")
    }
}

#[cfg(feature = "std")]
impl std::error::Error for OmsError {}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::collections::BTreeSet;
    use alloc::format;
    use alloc::string::ToString;
    use alloc::vec;

    #[cfg(not(feature = "std"))]
    #[test]
    fn native_id_generator_can_only_be_seeded_once() {
        assert_eq!(seed_id_generator(0x1234), Ok(()));
        assert_eq!(
            seed_id_generator(0x5678),
            Err(IdGeneratorAlreadyInitialized)
        );
    }

    #[test]
    fn generated_ids_are_unique() {
        let ids = (0..10_000)
            .map(|_| ObjectId::new())
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 10_000);
    }

    #[test]
    fn object_version_is_monotonic() {
        let initial = ObjectVersion::default();
        assert!(initial.next() > initial);
    }

    #[test]
    fn identifiers_round_trip_through_hex() {
        let id = ObjectId::new();
        assert_eq!(id.to_string().parse::<ObjectId>(), Ok(id));
        assert_eq!(format!("0x{id}").parse::<ObjectId>(), Ok(id));
        assert!("not-hex".parse::<ObjectId>().is_err());
    }

    #[test]
    fn recursive_values_round_trip() {
        let value = Value::Record(BTreeMap::from([
            ("name".to_owned(), Value::Text("Ousject".to_owned())),
            (
                "items".to_owned(),
                Value::Array(vec![
                    Value::Integer(1),
                    Value::Float(FloatValue::new(2.5)),
                    Value::Bytes(vec![3, 4]),
                ]),
            ),
            (
                "error".to_owned(),
                Value::Error {
                    code: "demo".to_owned(),
                    message: "expected".to_owned(),
                },
            ),
        ]));
        assert_eq!(Value::decode(&value.encode().unwrap()), Ok(value));
    }

    #[test]
    fn object_ids_are_text_and_old_versions_are_rejected() {
        let id = ObjectId::new();
        let value = Value::Array(vec![Value::Text(id.to_string())]);
        let encoded = value.encode().unwrap();
        assert!(encoded.starts_with(b"OVL0"));
        assert_eq!(Value::decode(&encoded), Ok(value));

        let mut old = Value::Integer(7).encode().unwrap();
        old[..4].copy_from_slice(b"OVL2");
        assert_eq!(Value::decode(&old), Err(ValueError::InvalidMagic));
    }

    #[test]
    fn value_truthiness_covers_collections() {
        assert!(!Value::Array(Vec::new()).is_truthy());
        assert!(Value::Array(vec![Value::Null]).is_truthy());
        assert!(!Value::Float(FloatValue::new(0.0)).is_truthy());
        assert!(Value::Float(FloatValue::new(0.5)).is_truthy());
    }
}
