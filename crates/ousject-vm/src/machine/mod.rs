//! Ousject TF virtual machine backed by Process and Variable Objects.

use crate::execution_core::{
    CallFrame, ExceptionHandler, ProcessState, ProcessStatus, TokenHost, VmError, WaitReason,
    WorkerLease, apply_halt, execute_token,
};
use crate::execution_core::{
    arithmetic, compare, execute_collection_token, math_capability, text_capability,
};
use oms_runtime::{
    AccessContext, CreateObject, CreateSpec, CreationPolicy, InMemoryObjectManager, ObjectQuery,
    ObjectView, Transaction, TypeDescriptor, ValueSchema,
};
use oms_types::{
    CORE_AUTHENTICATION_TYPE, CORE_CHANNEL_TYPE, CORE_COMPILER_TYPE, CORE_CRYPTO_TYPE,
    CORE_EFFECT_TYPE, CORE_INSTANCE_TYPE, CORE_MATH_TYPE, CORE_MODULE_INSTANCE_TYPE,
    CORE_MODULE_REGISTRY_TYPE, CORE_MODULE_TYPE, CORE_NAMESPACE_TYPE, CORE_OBJECT_STORE_TYPE,
    CORE_PACKAGE_AUDIT_TYPE, CORE_PACKAGE_DATA_TYPE, CORE_PACKAGE_DOWNLOAD_TYPE,
    CORE_PACKAGE_INSTALLATION_TYPE, CORE_PACKAGE_INSTANCE_TYPE, CORE_PACKAGE_MARKET_CONFIG_TYPE,
    CORE_PACKAGE_MARKET_TYPE, CORE_PACKAGE_MODULE_TYPE, CORE_PACKAGE_REGISTRY_TYPE,
    CORE_PACKAGE_SUBJECT_TYPE, CORE_PACKAGE_TYPE, CORE_PROCESS_TYPE, CORE_PROGRAM_TYPE,
    CORE_PROVIDER_REGISTRY_TYPE, CORE_SESSION_TYPE, CORE_SYSTEM_TYPE, CORE_TERMINAL_TYPE,
    CORE_TEXT_TYPE, CORE_TIME_TYPE, CORE_TYPE_REGISTRY_TYPE, CORE_USER_REGISTRY_TYPE,
    CORE_USER_TYPE, CORE_VALUE_TYPE, Capability, DEVICE_BLOCK_STORAGE_TYPE, DEVICE_DISPLAY_TYPE,
    DEVICE_KEYBOARD_TYPE, DEVICE_SENSOR_TYPE, LOCAL_USER_NAME, LifecycleState, NET_ENDPOINT_TYPE,
    NET_RESOLVER_TYPE, ObjectId, ObjectVersion, OmsError, SubjectId, TypeId,
};
use ousject_auth::{AuthService, UserIdentity};
use ousject_provider::{
    EffectRecord, ObjectProvider, ProviderError, ProviderOutcome, ProviderRegistry,
};
pub use ousject_provider::{EffectRecoveryPolicy, EffectStatus};
use praxis_compiler::{
    compile_interactive_with_contextual_loader, compile_program, compile_with_contextual_loader,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;
use std::sync::{Arc, Mutex, mpsc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tf_format::{Program, Token, Value};

pub use oms_types::SYSTEM_SUBJECT;
pub const PROGRAM_TYPE: TypeId = CORE_PROGRAM_TYPE;
pub const PROCESS_TYPE: TypeId = CORE_PROCESS_TYPE;
pub const INSTANCE_TYPE: TypeId = CORE_INSTANCE_TYPE;

const PROCESS_MAGIC: &[u8; 4] = b"OPS0";
const PROCESS_RESULT_EXTENSION: &[u8; 4] = b"PRX0";
const PROCESS_RUNTIME_EXTENSION: &[u8; 4] = b"PXT0";
const PROCESS_SCHEDULER_EXTENSION: &[u8; 4] = b"PSX0";
const PROCESS_RETENTION_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
const MAX_STATE_ITEMS: usize = 1_000_000;
type ProgramCache = BTreeMap<ObjectId, (oms_types::ObjectVersion, Arc<Program>)>;
type PackageVerificationCache = BTreeMap<ObjectId, (ObjectVersion, ObjectVersion)>;

include!("types.rs");
include!("providers.rs");
include!("vm_type.rs");
mod audit;
mod boot;
mod calls;
mod instruction;
mod lifecycle;
mod modules;
mod object_api;
mod object_invoke;
mod package_artifacts;
mod package_audit;
mod package_installations;
mod package_market;
mod package_support;
mod process_objects;
mod provider_dispatch;
mod registry;
mod swap_pool;
mod terminal;
mod timer;
mod transaction;
include!("bindings.rs");
include!("scheduler.rs");
include!("errors.rs");
include!("process_codec.rs");
include!("process_reaper.rs");
include!("object_inspection.rs");
include!("state_codec.rs");
include!("tests.rs");
