use oms_runtime::{
    AccessContext, CreateObject, CreateSpec, InMemoryObjectManager, ObjectQuery, SnapshotBackend,
};
use oms_types::{
    CORE_EFFECT_TYPE, CORE_MODULE_INSTANCE_TYPE, CORE_MODULE_TYPE, CORE_SESSION_TYPE,
    CORE_TERMINAL_TYPE, CORE_VALUE_TYPE, Capability, LifecycleState, NET_ENDPOINT_TYPE, ObjectId,
    OmsError, SubjectId, TypeId,
};
use ousject_provider::{
    EffectRecord, EffectRecoveryPolicy, EffectStatus, ObjectProvider, ProviderError,
    ProviderOutcome,
};
use ousject_vm::{
    CooperativeScheduler, ProcessStatus, SYSTEM_SUBJECT, TerminalProvider, VirtualMachine, VmError,
};
use praxis_compiler::{compile, compile_program};
use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tf_format::Value;

#[derive(Debug)]
struct TestTerminal;

impl TerminalProvider for TestTerminal {
    fn println(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }
}

fn vm_with_terminal(manager: Arc<InMemoryObjectManager>) -> VirtualMachine {
    let terminal =
        VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
    VirtualMachine::with_terminal(manager, terminal, Arc::new(TestTerminal)).unwrap()
}

fn vm_with_shell_terminal(manager: Arc<InMemoryObjectManager>) -> VirtualMachine {
    vm_with_terminal(manager)
}

mod atomic_objects;
mod audit;
mod durable_effects;
mod input_terminal;
mod ipc_recovery;
mod language;
mod math;
mod package_lifecycle;
mod persistent_terminal;
mod recovery;
mod security_network;

use durable_effects::FaultBackend;
use input_terminal::InputTerminal;
mod services;
mod shell_auth;
mod swap_pool;
