use nix::sys::termios::{
    ControlFlags, InputFlags, LocalFlags, SetArg, SpecialCharacterIndices, Termios, tcgetattr,
    tcsetattr,
};
use oms_runtime::{
    AccessContext, CreateObject, CreateSpec, CreationPolicy, InMemoryObjectManager, ObjectQuery,
    TombstoneReaper, ValueSchema,
};
use oms_types::{
    CORE_CONSOLE_TYPE, CORE_NAMESPACE_TYPE, CORE_PROGRAM_TYPE, CORE_SYSTEM_TYPE,
    CORE_TERMINAL_TYPE, Capability, DEVICE_BLOCK_STORAGE_TYPE, DEVICE_KEYBOARD_TYPE,
    NET_RESOLVER_TYPE, ObjectId, SubjectId, Value,
};
use ousject_auth::AuthService;
use ousject_provider::{
    EffectRecoveryPolicy, ObjectProvider, ProviderError, ProviderOutcome, TerminalScreen,
};
use ousject_vm::{
    ConsoleProvider, CooperativeScheduler, ProcessReaper, ProcessStatus, RunReport, SYSTEM_SUBJECT,
    VirtualMachine,
};
use praxis_compiler::compile_program_with_loader;
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fs::OpenOptions;
use std::io::{IsTerminal, Read, Seek, SeekFrom, Write};
use std::net::{Shutdown, TcpListener, TcpStream, ToSocketAddrs};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::Duration;
use tf_format::Program;

mod commands;

const DEFAULT_STEP_LIMIT: u64 = 1_000_000;
mod hardware;
mod options;
mod runtime;
mod terminal;
#[cfg(test)]
mod tests;

pub(crate) use commands::run_cli;
pub(super) use hardware::{
    CachedProvider, HostBlockStorageProvider, HostKeyboardProvider, HostNetworkProvider,
    HostResolverProvider, HostTerminalProvider,
};
pub(super) use options::{RuntimeOptions, open_manager, option_subject, parse_options};
pub(super) use runtime::{
    compile_source_file, discover_host_hardware, error_text, grant_console_access, host_block_path,
    parse_object, print_report, run_hosted_process,
};
pub(super) use terminal::LinuxConsole;
#[cfg(test)]
pub(super) use terminal::{InputMode, InputParser, KeyEvent, LinuxInputState, dispatch_event};
