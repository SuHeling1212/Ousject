use oms_runtime::{AccessContext, CreateSpec, InMemoryObjectManager, ObjectQuery, SnapshotBackend};
use oms_types::{
    CORE_CONSOLE_TYPE, CORE_EFFECT_TYPE, CORE_MODULE_INSTANCE_TYPE, CORE_MODULE_TYPE,
    CORE_SESSION_TYPE, CORE_TERMINAL_SESSION_TYPE, CORE_VALUE_TYPE, Capability, LifecycleState,
    NET_ENDPOINT_TYPE, ObjectId, OmsError, SubjectId, TypeId,
};
use ousject_provider::{
    EffectRecord, EffectRecoveryPolicy, EffectStatus, ObjectProvider, ProviderError,
    ProviderOutcome,
};
use ousject_vm::{
    ConsoleProvider, CooperativeScheduler, ProcessStatus, SYSTEM_SUBJECT, VirtualMachine, VmError,
};
use praxis_compiler::{compile, compile_program};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tf_format::Value;

#[derive(Debug)]
struct TestConsole;

impl ConsoleProvider for TestConsole {
    fn println(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }
}

fn vm_with_console(manager: Arc<InMemoryObjectManager>) -> VirtualMachine {
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    VirtualMachine::with_console(manager, console, Arc::new(TestConsole)).unwrap()
}

mod atomic_objects;
mod audit;
mod durable_effects;
mod input_console;
mod ipc_recovery;
mod language;
mod math;
mod package_lifecycle;
mod persistent_terminal;
mod recovery;
mod security_network;

use durable_effects::FaultBackend;
use input_console::InputConsole;
mod services;
mod shell_auth;
mod swap_pool;
