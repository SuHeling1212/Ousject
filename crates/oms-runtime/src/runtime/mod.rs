//! Object Management System core.
//!
//! Transactions lock participating state in a stable global order, validate a
//! complete candidate, make it durable, and only then publish it. Readers can
//! therefore never observe a partially published cross-shard transaction.

extern crate alloc;

#[cfg(not(feature = "std"))]
use self::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};
use alloc::borrow::ToOwned;
use alloc::boxed::Box;
use alloc::collections::{BTreeMap, BTreeSet};
use alloc::string::{String, ToString};
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering};
use core::time::Duration;
#[cfg(feature = "std")]
use im::{OrdMap, OrdSet};
use oms_shard::FixedDirectory;
use oms_types::{
    CORE_AUTHENTICATION_TYPE, CORE_BYTES_TYPE, CORE_CHANNEL_TYPE, CORE_COLLECTION_TYPE,
    CORE_COMPILER_TYPE, CORE_EFFECT_TYPE, CORE_INSTANCE_TYPE, CORE_MATH_TYPE,
    CORE_MODULE_INSTANCE_TYPE, CORE_MODULE_REGISTRY_TYPE, CORE_MODULE_TYPE, CORE_NAMESPACE_TYPE,
    CORE_OBJECT_STORE_TYPE, CORE_PROCESS_TYPE, CORE_PROGRAM_TYPE, CORE_PROVIDER_REGISTRY_TYPE,
    CORE_SCHEDULER_TYPE, CORE_SESSION_TYPE, CORE_SYSTEM_TYPE, CORE_TERMINAL_TYPE, CORE_TEXT_TYPE,
    CORE_TIME_TYPE, CORE_TYPE_REGISTRY_TYPE, CORE_USER_REGISTRY_TYPE, CORE_VALUE_TYPE, Capability,
    DEVICE_BLOCK_STORAGE_TYPE, DEVICE_DISPLAY_TYPE, DEVICE_KEYBOARD_TYPE, DEVICE_SENSOR_TYPE,
    LifecycleState, NET_ENDPOINT_TYPE, NET_RESOLVER_TYPE, ObjectHeader, ObjectId, ObjectVersion,
    OmsError, SYSTEM_SUBJECT, ShardId, SubjectId, TYPE_DESCRIPTOR_TYPE, TransactionId, TypeId,
    Value,
};
#[cfg(feature = "std")]
use std::fs::{self, File, OpenOptions};
#[cfg(feature = "std")]
use std::io::Write;
#[cfg(feature = "std")]
use std::path::{Path, PathBuf};
#[cfg(feature = "std")]
use std::sync::atomic::AtomicBool;
#[cfg(feature = "std")]
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
#[cfg(feature = "std")]
use std::sync::{Mutex, RwLock, RwLockReadGuard, RwLockWriteGuard};
#[cfg(feature = "std")]
use std::thread::{self, JoinHandle};
#[cfg(feature = "std")]
use std::time::{Instant, SystemTime, UNIX_EPOCH};
#[cfg(not(feature = "std"))]
type OrdMap<K, V> = BTreeMap<K, V>;
#[cfg(not(feature = "std"))]
type OrdSet<T> = BTreeSet<T>;

const SNAPSHOT_MAGIC: &[u8; 4] = b"OMS0";
#[cfg(feature = "std")]
const WAL_MAGIC: &[u8; 4] = b"OMW0";
#[cfg(feature = "std")]
const MANIFEST_MAGIC: &[u8; 4] = b"OMG0";
const RETIREMENT_TIME_EXTENSION: &[u8; 4] = b"RTM0";
const MAX_SNAPSHOT_ITEMS: usize = 16 * 1024 * 1024;
#[cfg(feature = "std")]
const CHECKPOINT_WAL_BYTES: u64 = 64 * 1024 * 1024;
const TOMBSTONE_RETENTION_MILLIS: u64 = 7 * 24 * 60 * 60 * 1_000;
#[cfg(feature = "std")]
const TOMBSTONE_REAPER_RETRY: Duration = Duration::from_secs(60);

include!("types.rs");
include!("objects.rs");
include!("transaction.rs");
#[cfg(feature = "std")]
include!("retention.rs");
#[cfg(feature = "std")]
include!("backend.rs");
#[cfg(not(feature = "std"))]
include!("backend_native.rs");
include!("manager_state.rs");
include!("manager.rs");
include!("retention_ops.rs");
include!("transaction_apply.rs");
include!("persistence.rs");
#[cfg(feature = "std")]
include!("tests.rs");

#[cfg(not(feature = "std"))]
mod sync;
