use oms_runtime::{AccessContext, CreateSpec, InMemoryObjectManager, ObjectQuery, SnapshotBackend};
use oms_types::{
    CORE_CONSOLE_TYPE, CORE_EFFECT_TYPE, CORE_SESSION_TYPE, CORE_VALUE_TYPE, Capability,
    LifecycleState, NET_ENDPOINT_TYPE, ObjectId, OmsError, SubjectId, TypeId,
};
use ousject_provider::{EffectRecord, ObjectProvider, ProviderError, ProviderOutcome};
use ousject_vm::{ConsoleProvider, ProcessStatus, SYSTEM_SUBJECT, VirtualMachine, VmError};
use praxis_compiler::compile;
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

#[derive(Debug)]
struct InputConsole {
    lines: Mutex<VecDeque<String>>,
    reads: AtomicUsize,
}

impl ConsoleProvider for InputConsole {
    fn println(&self, _text: &str) -> Result<(), String> {
        Ok(())
    }

    fn try_read_line(&self) -> Result<Option<String>, String> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Ok(self.lines.lock().unwrap().pop_front())
    }
}

#[test]
fn praxis_reads_console_input_through_a_durable_effect() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputConsole {
        lines: Mutex::new(VecDeque::from(["你好 Ousject".to_owned()])),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager, console, driver.clone()).unwrap();
    let program = compile(
        "console = object.find(\"console\")\nline = console.read_line()\nconsole.println(line)",
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 100).unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["你好 Ousject"]);
    assert_eq!(
        vm.variable(process, "line"),
        Ok(Value::Text("你好 Ousject".to_owned()))
    );
    assert_eq!(driver.reads.load(Ordering::SeqCst), 1);
}

#[test]
fn console_input_suspends_only_the_waiting_process_and_reuses_its_effect() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputConsole {
        lines: Mutex::new(VecDeque::new()),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager, console, driver.clone()).unwrap();
    let program = compile(
        "console = object.find(\"console\")\nline = console.read_line()\nconsole.println(line)",
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();

    let waiting = vm.run(process, 1_000).unwrap();
    assert_eq!(waiting.status, ProcessStatus::Suspended);
    let pending_effect = vm
        .manager()
        .read(AccessContext::new(SYSTEM_SUBJECT), process)
        .unwrap()
        .links()["$effect"];
    driver.lines.lock().unwrap().push_back("ready".to_owned());
    assert!(vm.poll_pending_effect(process).unwrap());
    let completed = vm.run(process, 1_000).unwrap();
    assert_eq!(completed.status, ProcessStatus::Halted);
    assert_eq!(completed.output, ["ready"]);
    assert_eq!(driver.reads.load(Ordering::SeqCst), 2);
    let effect = EffectRecord::decode(
        vm.manager()
            .read(AccessContext::new(SYSTEM_SUBJECT), pending_effect)
            .unwrap()
            .state(),
    )
    .unwrap();
    assert_eq!(effect.status, ousject_provider::EffectStatus::Completed);
    assert_eq!(effect.result, Some(Value::Text("ready".to_owned())));
}

#[test]
fn two_console_waiters_keep_distinct_effects_and_inputs() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputConsole {
        lines: Mutex::new(VecDeque::new()),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager, console, driver.clone()).unwrap();
    let program = compile(
        "console = object.find(\"console\")\nline = console.read_line()\nconsole.println(line)",
    )
    .unwrap();
    let first = vm.create_process(&program).unwrap();
    let second = vm.create_process(&program).unwrap();
    assert_eq!(
        vm.run(first, 1_000).unwrap().status,
        ProcessStatus::Suspended
    );
    assert_eq!(
        vm.run(second, 1_000).unwrap().status,
        ProcessStatus::Suspended
    );
    let context = AccessContext::new(SYSTEM_SUBJECT);
    let first_effect = vm.manager().read(context, first).unwrap().links()["$effect"];
    let second_effect = vm.manager().read(context, second).unwrap().links()["$effect"];
    assert_ne!(first_effect, second_effect);

    driver
        .lines
        .lock()
        .unwrap()
        .extend(["first".to_owned(), "second".to_owned()]);
    assert!(vm.poll_pending_effect(first).unwrap());
    assert!(vm.poll_pending_effect(second).unwrap());
    assert_eq!(vm.run(first, 1_000).unwrap().output, ["first"]);
    assert_eq!(vm.run(second, 1_000).unwrap().output, ["second"]);
}

#[test]
fn pending_console_input_recovers_after_store_reopen() {
    let directory =
        std::env::temp_dir().join(format!("ousject-input-recovery-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let process;
    let console;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        console =
            VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
        let driver = Arc::new(InputConsole {
            lines: Mutex::new(VecDeque::new()),
            reads: AtomicUsize::new(0),
        });
        let vm = VirtualMachine::with_console(manager, console, driver).unwrap();
        let program = compile(
            "console = object.find(\"console\")\nline = console.read_line()\nconsole.println(line)",
        )
        .unwrap();
        process = vm.create_process(&program).unwrap();
        assert_eq!(
            vm.run(process, 1_000).unwrap().status,
            ProcessStatus::Suspended
        );
    }

    let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
    let driver = Arc::new(InputConsole {
        lines: Mutex::new(VecDeque::from(["after restart".to_owned()])),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager, console, driver).unwrap();
    assert!(vm.poll_pending_effect(process).unwrap());
    let report = vm.run(process, 1_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["after restart"]);
    drop(vm);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn praxis_discovers_and_uses_kernel_service_objects() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let vm = vm_with_console(Arc::clone(&manager));
    let program = compile(
        r#"
console = object.find("console")
system = object.find("system")
store = object.find("store")
types = object.find("types")
processes = object.query("core.process")
console.println(system.status())
console.println(store.health_check())
console.println(types.types())
console.println(processes)
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 1_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output.len(), 4);
    assert!(report.output[0].contains("running"), "{:?}", report.output);
    assert!(report.output[1].contains("healthy"));
    assert!(report.output[2].contains("core.system"));
    assert!(report.output[3].contains(&process.to_string()));
}

#[test]
fn praxis_compiles_and_executes_program_objects_through_compiler_service() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(
        r#"
console = object.find("console")
compiler = object.find("compiler")
source = "value = 40 + 2"
console.println(compiler.validate(source))
program_id = compiler.compile(source)
program = object.find(program_id)
console.println(program.type)
child_id = program.execute()
child = object.find(child_id)
console.println(child.wait())
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 2_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["true", "core.program", "halted"]);
}

#[test]
fn process_wait_keeps_driving_a_child_suspended_for_console_input() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputConsole {
        lines: Mutex::new(VecDeque::new()),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager, console, driver.clone()).unwrap();
    let delayed_input = Arc::clone(&driver);
    let input_thread = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(30));
        delayed_input
            .lines
            .lock()
            .unwrap()
            .push_back("ready".to_owned());
    });
    let program = compile(
        r#"
console = object.find("console")
compiler = object.find("compiler")
source = "console = object.find(\"console\")\nconsole.read_line()"
program_id = compiler.compile(source)
child_program = object.find(program_id)
child_id = child_program.execute()
child = object.find(child_id)
console.println(child.wait())
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();

    let report = vm.run(process, 2_000).unwrap();
    input_thread.join().unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["halted"]);
    assert!(driver.reads.load(Ordering::SeqCst) >= 2);
}

#[test]
fn praxis_initializes_local_and_logs_in_through_authentication_objects() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(
        r#"
console = object.find("console")
authentication = object.find("authentication")
users = object.find("users")
authentication.initialize_local("local-password")
users.create_user("alice", "alice-password")
session = authentication.login("alice", "alice-password")
console.println(session["subject"])
authentication.change_password("alice", "new-password")
authentication.logout(session["token"])
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 2_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output.len(), 1);
    assert_ne!(report.output[0], SYSTEM_SUBJECT.to_string());
    assert_eq!(
        vm.process_state(process).unwrap().subject.to_string(),
        report.output[0]
    );
}

#[test]
fn shell_context_keeps_variables_and_current_user_between_command_processes() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let source = r#"
console = object.find("console")
authentication = object.find("authentication")
users = object.find("users")
authentication.initialize_local("local-password")
users.create_user("alice", "alice-password")
authentication.login("alice", "alice-password")
identity = authentication.current_user()
console.println(identity["name"])
context = {}
compiler = object.find("compiler")
first_id = compiler.compile("a = 123456789")
first_program = object.find(first_id)
first_process_id = first_program.execute(context)
first_process = object.find(first_process_id)
first_process.wait()
context = first_process.bindings()
second_id = compiler.compile("b = a + 1")
second_program = object.find(second_id)
second_process_id = second_program.execute(context)
second_process = object.find(second_process_id)
second_process.wait()
second_context = second_process.bindings()
b_object = object.find(second_context["b"])
console.println(b_object.value)
"#;
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 10_000).unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["alice", "123456790"]);
    let subject = vm.process_state(process).unwrap().subject;
    assert_ne!(subject, SYSTEM_SUBJECT);
}

#[test]
fn system_shell_collects_bracket_and_backslash_continuations() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(InputConsole {
        lines: Mutex::new(VecDeque::from([
            "a = {".to_owned(),
            "x: 3".to_owned(),
            "}".to_owned(),
            "b = 4 + \\".to_owned(),
            "5".to_owned(),
            "objects".to_owned(),
            "exit".to_owned(),
        ])),
        reads: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager.clone(), console, driver.clone()).unwrap();
    let setup = vm
        .create_process(
            &compile(
                "authentication = object.find(\"authentication\")\nauthentication.initialize_local(\"local-password\")",
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(vm.run(setup, 1_000).unwrap().status, ProcessStatus::Halted);

    let shell = vm
        .create_process(&compile(include_str!("../../../system/shell.px")).unwrap())
        .unwrap();
    let report = vm.run(shell, 100_000).unwrap();

    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(driver.reads.load(Ordering::SeqCst), 7);
    assert!(
        report
            .output
            .iter()
            .all(|line| !line.starts_with("provider_error")),
        "Shell reported an error: {:?}",
        report.output
    );
    let context = vm.variable(shell, "context").unwrap();
    let (Value::Map(context) | Value::Record(context)) = context else {
        panic!("Shell context should be a map of variable names to Object IDs");
    };
    assert!(
        context.contains_key("a"),
        "Shell context {context:?}; output {:?}",
        report.output
    );
    assert!(
        context.contains_key("b"),
        "Shell context {context:?}; output {:?}",
        report.output
    );
    let Value::Text(a_id) = &context["a"] else {
        panic!("variable a should be bound to an Object ID");
    };
    let Value::Text(b_id) = &context["b"] else {
        panic!("variable b should be bound to an Object ID");
    };
    let access = AccessContext::new(SYSTEM_SUBJECT);
    let a = manager.value(access, a_id.parse().unwrap()).unwrap();
    let b = manager.value(access, b_id.parse().unwrap()).unwrap();
    assert_eq!(
        a,
        Value::Map(BTreeMap::from([("x".to_owned(), Value::Integer(3))]))
    );
    assert_eq!(b, Value::Integer(9));
}

#[test]
fn praxis_uses_math_object_capabilities_and_constants() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let source = r#"
math = object.find("math")
pi = math.pi
e = math.e
absolute = math.abs(-7)
minimum = math.min(2, 5)
maximum = math.max(4, 4.5)
clamped = math.clamp(9, 0, 5)
root = math.sqrt(81)
power = math.pow(2, 3)
floor_value = math.floor(3.9)
ceil_value = math.ceil(3.1)
rounded = math.round(-2.5)
truncated = math.trunc(-2.9)
sine = math.sin(0)
cosine = math.cos(0)
tangent = math.tan(0)
angle = math.atan2(1, 0)
natural_log = math.log(e)
binary_log = math.log2(8)
decimal_log = math.log10(100)
exponential = math.exp(0)
distance = math.hypot(3, 4)
"#;
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    assert_eq!(
        vm.run(process, 5_000).unwrap().status,
        ProcessStatus::Halted
    );

    for (name, expected) in [
        ("pi", std::f64::consts::PI),
        ("e", std::f64::consts::E),
        ("maximum", 4.5),
        ("root", 9.0),
        ("power", 8.0),
        ("floor_value", 3.0),
        ("ceil_value", 4.0),
        ("rounded", -3.0),
        ("truncated", -2.0),
        ("sine", 0.0),
        ("cosine", 1.0),
        ("tangent", 0.0),
        ("angle", std::f64::consts::FRAC_PI_2),
        ("natural_log", 1.0),
        ("binary_log", 3.0),
        ("decimal_log", 2.0),
        ("exponential", 1.0),
        ("distance", 5.0),
    ] {
        assert_eq!(
            vm.variable(process, name),
            Ok(Value::Float(oms_types::FloatValue::new(expected))),
            "unexpected math result for {name}"
        );
    }
    assert_eq!(vm.variable(process, "absolute"), Ok(Value::Integer(7)));
    assert_eq!(vm.variable(process, "minimum"), Ok(Value::Integer(2)));
    assert_eq!(vm.variable(process, "clamped"), Ok(Value::Integer(5)));
}

#[test]
fn math_object_reports_domain_and_overflow_errors() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let negative_root = vm
        .create_process(&compile("math = object.find(\"math\")\nmath.sqrt(-1)").unwrap())
        .unwrap();
    assert!(matches!(
        vm.run(negative_root, 100),
        Err(ousject_vm::VmError::TypeError(_))
    ));

    let overflow = vm
        .create_process(&compile("math = object.find(\"math\")\nmath.exp(10000)").unwrap())
        .unwrap();
    assert!(matches!(
        vm.run(overflow, 100),
        Err(ousject_vm::VmError::TypeError(_))
    ));
}

#[test]
fn praxis_login_and_process_identity_transition_are_one_atomic_commit() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager.clone());
    let program = compile(
        r#"
authentication = object.find("authentication")
users = object.find("users")
authentication.initialize_local("local-password")
users.create_user("alice", "alice-password")
session = authentication.login("alice", "alice-password")
"#,
    )
    .unwrap();
    let login_position = u32::try_from(
        program
            .tokens
            .iter()
            .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "login"))
            .unwrap(),
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    while vm.process_state(process).unwrap().token_position != login_position {
        assert_eq!(vm.run(process, 1).unwrap().status, ProcessStatus::Running);
    }
    let before = vm.process_state(process).unwrap();

    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    assert!(
        manager
            .query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(CORE_SESSION_TYPE),
            )
            .unwrap()
            .is_empty()
    );

    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    assert_ne!(vm.process_state(process).unwrap().subject, SYSTEM_SUBJECT);
    assert_eq!(
        manager
            .query(
                AccessContext::new(SYSTEM_SUBJECT),
                &ObjectQuery::new().with_type(CORE_SESSION_TYPE),
            )
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn reserved_local_user_cannot_be_replaced_or_retired_by_praxis() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(Arc::clone(&manager));
    let program = compile(
        r#"
console = object.find("console")
authentication = object.find("authentication")
authentication.initialize_local("password")
found = object.query("core.user")
local = object.find(found[0])
try {
    local.replace({ name: "forged" })
} catch (error) {
    console.println("replace denied")
}
try {
    local.retire()
} catch (error) {
    console.println("retire denied")
}
"#,
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    let report = vm.run(process, 2_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, ["replace denied", "retire denied"]);
    let users = ousject_auth::AuthService::new(manager).users().unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].name, "local");
    assert_eq!(users[0].subject, SYSTEM_SUBJECT);
}

#[test]
fn praxis_process_recovers_and_finishes_after_restart() {
    let source = r#"
count = 0
console = object.find("console")
while count < 5 {
    count++
}
console.println(count)
"#;
    let program = compile(source).unwrap();
    let directory = std::env::temp_dir().join(format!("ousject-e2e-{}", ObjectId::new()));
    let path = directory.join("objects.oms");

    let process = {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_console(manager);
        let process = vm.create_process(&program).unwrap();
        let partial = vm.run(process, 7).unwrap();
        assert_eq!(partial.status, ProcessStatus::Running);
        assert!(partial.output.is_empty());
        process
    };

    let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
    let vm = vm_with_console(manager);
    vm.reconnect_hardware(process).unwrap();
    let completed = vm.run(process, 200).unwrap();
    assert_eq!(completed.status, ProcessStatus::Halted);
    assert_eq!(completed.output, vec!["5"]);
    assert_eq!(vm.variable(process, "count"), Ok(Value::Integer(5)));
    vm.manager().health_check().unwrap();
    drop(vm);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn extended_praxis_control_flow_runs_end_to_end() {
    let source = r#"
count = 0
sum = 0
console = object.find("console")

while count < 10 {
    count++
    if count % 2 != 0 {
        continue
    }
    sum = sum + count
    if sum > 10 {
        break
    }
}

count--

if false && missing {
    console.println("short circuit failed")
} else if true || missing {
    console.println(sum)
    console.println(count)
}
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 500).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, vec!["12", "5"]);
    assert_eq!(vm.variable(process, "sum"), Ok(Value::Integer(12)));
    assert_eq!(vm.variable(process, "count"), Ok(Value::Integer(5)));
}

#[test]
fn praxis_collections_run_and_persist_as_values() {
    let source = r#"
items = [1, 2, 3]
console = object.find("console")
items[0] = 10
user = {
    name: "Ada",
    age: 18,
}
user["age"] = 19
console.println(items[0] + user["age"])
console.println(#items + #user + #"hi")
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 500).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output, vec!["29", "7"]);
    assert_eq!(
        vm.variable(process, "items"),
        Ok(Value::Array(vec![
            Value::Integer(10),
            Value::Integer(2),
            Value::Integer(3),
        ]))
    );
}

#[test]
fn praxis_functions_and_class_objects_run_end_to_end() {
    let source = r#"
func twice(value) {
    return value * 2
}

class BaseCounter {
    value = 0
    private secret = 7

    func add(amount) {
        this.value = this.value + amount
        return this.value
    }

    func reveal() {
        return this.secret
    }
}

class Counter extends BaseCounter {
    func add(amount) {
        return super.add(twice(amount))
    }
}

console = object.find("console")
counter = object.create("Counter", { value: 10 })
same link counter
console.println(counter.add(3))
console.println(same.value)
console.println(counter.reveal())
console.println(counter.type)
console.println(counter.inspect["type"])
console.println(#object.query("Counter", "add"))
console.println(#counter.capabilities)
"#;
    let directory = std::env::temp_dir().join(format!("ousject-class-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let process;
    let counter;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_console(manager);
        process = vm.create_process(&compile(source).unwrap()).unwrap();
        let report = vm.run(process, 1_000).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted);
        assert_eq!(
            report.output,
            vec!["16", "16", "7", "Counter", "Counter", "1", "11"]
        );
        let state = vm.process_state(process).unwrap();
        counter = state.variables["counter"];
        assert_eq!(state.variables["same"], counter);
        assert_eq!(vm.variable(process, "counter").unwrap().kind(), "record");
    }

    let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
    assert_eq!(
        manager
            .value(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), counter)
            .unwrap(),
        Value::Record(BTreeMap::from([
            ("$class".to_owned(), Value::Text("Counter".to_owned())),
            ("secret".to_owned(), Value::Integer(7)),
            ("value".to_owned(), Value::Integer(16)),
        ]))
    );
    drop(manager);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn class_private_fields_are_not_visible_outside_methods() {
    let source = r#"
class Secret {
    private value = 1
}
secret = object.create("Secret", {})
copy = secret.value
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    assert!(matches!(
        vm.run(process, 100),
        Err(ousject_vm::VmError::TypeError(
            "private field is not visible"
        ))
    ));
}

#[test]
fn praxis_index_updates_unicode_and_try_catch_run_end_to_end() {
    let source = r#"
console = object.find("console")
items = [1, 4]
items[0]++
items[1]--
console.println(items[0] + items[1])
console.println("你好，Ousject")

try {
    failed = 1 / 0
    console.println("not reached")
} catch (error) {
    console.println(error)
}

console.println("continued")
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 500).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(report.output[0], "5");
    assert_eq!(report.output[1], "你好，Ousject");
    assert!(report.output[2].contains("division_by_zero"));
    assert_eq!(report.output[3], "continued");
    assert_eq!(
        vm.variable(process, "items"),
        Ok(Value::Array(vec![Value::Integer(2), Value::Integer(3)]))
    );
}

#[test]
fn praxis_object_api_runs_and_recovers() {
    let source = r#"
	console = object.find("console")
	item = object.create("core.text", "one")
	same = object.find(item.id)
	console.println(item.type)
	console.println(item.value)
	item.replace("two")
	root = object.create("core.namespace", {})
	child = object.create("core.text", "nested", root)
	parent = child.parent
	console.println(parent == root.id)
	console.println(#root.children)
	root.link("item", item)
	found = root.links["item"]
	found_object = object.find(found)
	console.println(found_object.value)
	console.println(#object.query("core.text"))
	console.println(item.inspect["type"])
	console.println(item.status)
	console.println(#item.capabilities)
	root.unlink("item")
	console.println(#root.links)
"#;
    let directory = std::env::temp_dir().join(format!("ousject-object-api-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let process;
    let item;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_console(manager);
        process = vm.create_process(&compile(source).unwrap()).unwrap();
        let report = vm.run(process, 500).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted);
        assert_eq!(
            report.output,
            vec![
                "core.text",
                "one",
                "true",
                "1",
                "two",
                "2",
                "core.text",
                "Active",
                "17",
                "0"
            ]
        );
        item = vm.process_state(process).unwrap().variables["item"];
        assert_eq!(
            vm.variable(process, "item"),
            Ok(Value::Text("two".to_owned()))
        );
        vm.manager().health_check().unwrap();
    }
    let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
    assert_eq!(
        manager.value(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), item),
        Ok(Value::Text("two".to_owned()))
    );
    let vm = vm_with_console(manager);
    assert_eq!(
        vm.variable(process, "item"),
        Ok(Value::Text("two".to_owned()))
    );
    assert_eq!(vm.process_state(process).unwrap().variables["item"], item);
    drop(vm);
    std::fs::remove_dir_all(directory).unwrap();
}

#[test]
fn assignment_copies_value_into_an_independent_object() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm
        .create_process(
            &compile(
                "console = object.find(\"console\")\nitem = object.create(\"core.text\", \"one\")\ncopy = item\nconsole.println(copy == item)\nconsole.println(copy.value)",
            )
            .unwrap(),
        )
        .unwrap();
    assert_eq!(vm.run(process, 100).unwrap().output, vec!["true", "one"]);
    assert_eq!(
        vm.variable(process, "copy"),
        Ok(Value::Text("one".to_owned()))
    );
    let bindings = vm.process_state(process).unwrap().variables;
    assert_ne!(bindings["item"], bindings["copy"]);
}

#[test]
fn assignment_and_explicit_core_value_creation_bind_the_same_kind_of_object() {
    let source = r#"
x = 46
note = object.create("core.value", 42)
hex_text = "00000000000000000000000000000001"
aaa = object.find("console")
aaa.println(x + 1)
aaa.println(note + 1)
aaa.println(x.type)
aaa.println(note.type)
aaa.println(x.value)
aaa.println(note.value)
aaa.println(hex_text.value)
aaa.println("named console object")
"#;
    let program = compile(source).unwrap();
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&program).unwrap();
    assert_eq!(
        vm.run(process, 200).unwrap().output,
        vec![
            "47",
            "43",
            "core.value",
            "core.value",
            "46",
            "42",
            "00000000000000000000000000000001",
            "named console object"
        ]
    );
    let state = vm.process_state(process).unwrap();
    let context = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);
    for name in ["x", "note"] {
        let header = vm
            .manager()
            .inspect(context, state.variables[name])
            .unwrap();
        assert_eq!(header.type_id, CORE_VALUE_TYPE);
        assert_eq!(header.parent_id, Some(process));
    }
    assert_eq!(vm.variable(process, "x"), Ok(Value::Integer(46)));
    assert_eq!(vm.variable(process, "note"), Ok(Value::Integer(42)));
    let console = vm.manager().read(context, process).unwrap().links()["console"];
    assert_eq!(state.variables["aaa"], console);
    assert_eq!(
        vm.manager().inspect(context, console).unwrap().type_id,
        CORE_CONSOLE_TYPE
    );
}

#[derive(Debug, Default)]
struct FaultBackend {
    bytes: Mutex<Option<Vec<u8>>>,
    fail_next: AtomicBool,
}

impl SnapshotBackend for FaultBackend {
    fn load(&self) -> Result<Option<Vec<u8>>, OmsError> {
        Ok(self.bytes.lock().unwrap().clone())
    }

    fn store(&self, snapshot: &[u8]) -> Result<(), OmsError> {
        if self.fail_next.swap(false, Ordering::SeqCst) {
            return Err(OmsError::Storage("forced failure".to_owned()));
        }
        *self.bytes.lock().unwrap() = Some(snapshot.to_vec());
        Ok(())
    }
}

#[derive(Debug)]
struct FaultingConsole {
    backend: Arc<FaultBackend>,
    deliveries: AtomicUsize,
}

impl ConsoleProvider for FaultingConsole {
    fn println(&self, _text: &str) -> Result<(), String> {
        self.deliveries.fetch_add(1, Ordering::SeqCst);
        self.backend.fail_next.store(true, Ordering::SeqCst);
        Ok(())
    }
}

#[test]
fn console_output_has_a_durable_effect_and_is_not_repeated_after_completion_failure() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let console =
        VirtualMachine::publish_console(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let driver = Arc::new(FaultingConsole {
        backend,
        deliveries: AtomicUsize::new(0),
    });
    let vm = VirtualMachine::with_console(manager.clone(), console, driver.clone()).unwrap();
    let program =
        compile("console = object.find(\"console\")\nconsole.println(\"hello\")").unwrap();
    let call = program
        .tokens
        .iter()
        .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "println"))
        .unwrap();
    let process = vm.create_process(&program).unwrap();
    while usize::try_from(vm.process_state(process).unwrap().token_position).unwrap() < call {
        vm.run(process, 1).unwrap();
    }

    assert!(matches!(
        vm.run(process, 1),
        Err(VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(driver.deliveries.load(Ordering::SeqCst), 1);
    assert!(
        manager
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), process)
            .unwrap()
            .links()
            .contains_key("$effect")
    );

    let report = vm.run(process, 100).unwrap();
    assert_eq!(report.output, ["hello"]);
    assert_eq!(driver.deliveries.load(Ordering::SeqCst), 1);
}

#[derive(Debug)]
struct TestEffectProvider {
    backend: Arc<FaultBackend>,
    outcomes: Mutex<BTreeMap<ObjectId, ProviderOutcome>>,
    invocations: AtomicUsize,
}

impl ObjectProvider for TestEffectProvider {
    fn type_id(&self) -> TypeId {
        NET_ENDPOINT_TYPE
    }

    fn user_creatable(&self) -> bool {
        true
    }

    fn create(&self, _initial: &Value) -> Result<Value, ProviderError> {
        Ok(Value::Record(BTreeMap::from([(
            "status".to_owned(),
            Value::Text("new".to_owned()),
        )])))
    }

    fn invoke(
        &self,
        _object: ObjectId,
        _state: &Value,
        capability: &str,
        arguments: &[Value],
        effect: ObjectId,
    ) -> Result<ProviderOutcome, ProviderError> {
        if let Some(outcome) = self.outcomes.lock().unwrap().get(&effect).cloned() {
            return Ok(outcome);
        }
        assert_eq!(capability, "send");
        assert_eq!(arguments, [Value::Text("hello".to_owned())]);
        self.invocations.fetch_add(1, Ordering::SeqCst);
        let outcome =
            ProviderOutcome::result(Value::Integer(5)).with_state(Value::Record(BTreeMap::from([
                ("status".to_owned(), Value::Text("sent".to_owned())),
            ])));
        self.outcomes
            .lock()
            .unwrap()
            .insert(effect, outcome.clone());
        self.backend.fail_next.store(true, Ordering::SeqCst);
        Ok(outcome)
    }

    fn capabilities(&self) -> std::collections::BTreeSet<String> {
        ["send".to_owned()].into_iter().collect()
    }
}

#[test]
fn provider_effect_is_durable_and_idempotent_across_completion_failure() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = VirtualMachine::new(manager.clone());
    let provider = Arc::new(TestEffectProvider {
        backend: backend.clone(),
        outcomes: Mutex::new(BTreeMap::new()),
        invocations: AtomicUsize::new(0),
    });
    vm.register_provider(provider.clone()).unwrap();
    let program = compile(
        "endpoint = object.create(\"net.endpoint\", { transport: \"tcp\" })\nsent = endpoint.send(\"hello\")",
    )
    .unwrap();
    let call = program
        .tokens
        .iter()
        .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "send"))
        .unwrap();
    let process = vm.create_process(&program).unwrap();
    while usize::try_from(vm.process_state(process).unwrap().token_position).unwrap() < call {
        vm.run(process, 1).unwrap();
    }

    assert!(matches!(
        vm.run(process, 1),
        Err(VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(provider.invocations.load(Ordering::SeqCst), 1);
    let process_view = manager
        .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), process)
        .unwrap();
    let effect = process_view.links()["$effect"];
    assert_eq!(
        EffectRecord::decode(
            manager
                .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), effect)
                .unwrap()
                .state()
        )
        .unwrap()
        .status,
        ousject_provider::EffectStatus::Pending
    );

    assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
    assert_eq!(provider.invocations.load(Ordering::SeqCst), 1);
    assert_eq!(vm.variable(process, "sent"), Ok(Value::Integer(5)));
    assert!(
        !manager
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), process)
            .unwrap()
            .links()
            .contains_key("$effect")
    );
    let effects = manager
        .query(
            AccessContext::new(ousject_vm::SYSTEM_SUBJECT),
            &oms_runtime::ObjectQuery::new().with_type(CORE_EFFECT_TYPE),
        )
        .unwrap();
    assert_eq!(effects.len(), 1);
    assert_eq!(
        EffectRecord::decode(
            manager
                .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), effect)
                .unwrap()
                .state()
        )
        .unwrap()
        .status,
        ousject_provider::EffectStatus::Completed
    );
}

#[test]
fn praxis_object_creation_and_process_advance_are_atomic() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let process = vm
        .create_process(&compile("item = object.create(\"core.text\", \"x\")").unwrap())
        .unwrap();
    assert_eq!(vm.run(process, 2).unwrap().status, ProcessStatus::Running);
    let before = vm.process_state(process).unwrap();
    let count = vm.manager().stats().unwrap().object_count;
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    assert_eq!(vm.manager().stats().unwrap().object_count, count);
    assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
    let item = vm.process_state(process).unwrap().variables["item"];
    assert_eq!(
        vm.manager()
            .value(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), item),
        Ok(Value::Text("x".to_owned()))
    );
}

#[test]
fn praxis_object_replacement_and_process_advance_are_atomic() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let program =
        compile("item = object.create(\"core.text\", \"old\")\nitem.replace(\"new\")").unwrap();
    let replace_position = program
        .tokens
        .iter()
        .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "replace"))
        .unwrap();
    let process = vm.create_process(&program).unwrap();
    assert_eq!(
        vm.run(process, u64::try_from(replace_position).unwrap())
            .unwrap()
            .status,
        ProcessStatus::Running
    );
    let item = vm.process_state(process).unwrap().variables["item"];
    let before = vm.process_state(process).unwrap();
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    let reopened = InMemoryObjectManager::open_with_backend(backend).unwrap();
    assert_eq!(
        reopened.value(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), item),
        Ok(Value::Text("old".to_owned()))
    );
    assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.manager()
            .value(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), item),
        Ok(Value::Text("new".to_owned()))
    );
}

#[test]
fn praxis_link_and_process_advance_are_atomic() {
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(
        "root = object.create(\"core.namespace\", {})\nitem = object.create(\"core.text\", \"x\")\nroot.link(\"item\", item)",
    )
    .unwrap();
    let link_position = program
        .tokens
        .iter()
        .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "link"))
        .unwrap();
    let process = vm.create_process(&program).unwrap();
    vm.run(process, u64::try_from(link_position).unwrap())
        .unwrap();
    let root = vm.process_state(process).unwrap().variables["root"];
    let before = vm.process_state(process).unwrap();
    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    let reopened = InMemoryObjectManager::open_with_backend(backend).unwrap();
    assert!(
        reopened
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), root)
            .unwrap()
            .links()
            .is_empty()
    );
    assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
    assert!(
        vm.manager()
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), root)
            .unwrap()
            .links()
            .contains_key("item")
    );
}

#[test]
fn praxis_transaction_commits_multiple_objects_atomically() {
    let source = r#"
class Account { balance = 0 }
source = object.create("Account", { balance: 20 })
target = object.create("Account", { balance: 2 })
transaction {
    source.balance = source.balance - 10
    target.balance = target.balance + 10
}
"#;
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(source).unwrap();
    let transaction_position = u32::try_from(
        program
            .tokens
            .iter()
            .position(|token| matches!(token, tf_format::Token::Transaction { .. }))
            .unwrap(),
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    while vm.process_state(process).unwrap().token_position != transaction_position {
        assert_eq!(vm.run(process, 1).unwrap().status, ProcessStatus::Running);
    }
    let bindings = vm.process_state(process).unwrap().variables;
    let source = bindings["source"];
    let target = bindings["target"];
    let context = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);
    let before_process = vm.process_state(process).unwrap();

    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before_process));
    for (object, expected) in [(source, 20), (target, 2)] {
        let Value::Record(fields) = vm.manager().value(context, object).unwrap() else {
            panic!("expected Account state")
        };
        assert_eq!(fields["balance"], Value::Integer(expected));
    }

    assert_eq!(vm.run(process, 50).unwrap().status, ProcessStatus::Halted);
    for (object, expected) in [(source, 10), (target, 12)] {
        let Value::Record(fields) = vm.manager().value(context, object).unwrap() else {
            panic!("expected Account state")
        };
        assert_eq!(fields["balance"], Value::Integer(expected));
    }
}

#[test]
fn praxis_can_create_run_and_link_process_objects() {
    let source = r#"
class Channel { value = 0 }

func worker() {
    channel = object.find("channel")
    channel.value++
}

console = object.find("console")
self = object.find("process")
program = object.find("program")
channel = object.create("Channel", {})
child = object.create("core.process", {
    entry: "worker",
    links: { channel: channel.id }
})
console.println(self.type)
console.println(program.type)
console.println(child.status)
child.start()
console.println(child.wait())
console.println(channel.value)
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 1_000).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(
        report.output,
        vec!["core.process", "core.program", "suspended", "halted", "1"]
    );
    let child = vm.process_state(process).unwrap().variables["child"];
    assert_eq!(
        vm.process_state(child).unwrap().status,
        ProcessStatus::Halted
    );
}

#[test]
fn collection_uses_variable_value_capabilities() {
    let source = r#"
console = object.find("console")
items = object.create("core.collection", [1, 2])
items[1] = 9
console.println(items[1])
items.replace([7, 9, 11])
console.println(items[2])
console.println(#items)
console.println(items.value)

mapping = object.create("core.collection", {"a": 1})
mapping["b"] = 2
console.println(mapping["a"])
console.println(mapping["b"])
console.println(#mapping)
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    let report = vm.run(process, 500).unwrap();
    assert_eq!(report.status, ProcessStatus::Halted);
    assert_eq!(
        report.output,
        vec![
            "9",
            "11",
            "3",
            "[Integer(7), Integer(9), Integer(11)]",
            "1",
            "2",
            "2"
        ]
    );
}

#[test]
fn praxis_object_api_can_grant_permissions() {
    let subject = oms_types::SubjectId::from_u128(0xabc);
    let source = format!(
        "item = object.create(\"core.text\", \"shared\")\nitem.grant(\"{subject}\", \"view_value\")\n"
    );
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_console(manager);
    let process = vm.create_process(&compile(&source).unwrap()).unwrap();
    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    let item = vm.process_state(process).unwrap().variables["item"];
    assert_eq!(
        vm.manager().value(AccessContext::new(subject), item),
        Ok(Value::Text("shared".to_owned()))
    );
}

#[test]
fn retiring_an_object_atomically_cleans_names_links_and_children() {
    let source = r#"
root = object.create("core.namespace", {})
item = object.create("core.text", "old")
child = object.create("core.text", "child", item)
alias link item
root.link("item", item)
item.retire()
item = "fresh"
"#;
    let backend = Arc::new(FaultBackend::default());
    let manager = Arc::new(InMemoryObjectManager::open_with_backend(backend.clone()).unwrap());
    let vm = vm_with_console(manager);
    let program = compile(source).unwrap();
    let retire_position = u32::try_from(
        program
            .tokens
            .iter()
            .position(|token| matches!(token, tf_format::Token::ObjectCall { method, .. } if method == "retire"))
            .unwrap(),
    )
    .unwrap();
    let process = vm.create_process(&program).unwrap();
    while vm.process_state(process).unwrap().token_position != retire_position {
        assert_eq!(vm.run(process, 1).unwrap().status, ProcessStatus::Running);
    }
    let before = vm.process_state(process).unwrap();
    let old_item = before.variables["item"];
    let old_child = before.variables["child"];
    let root = before.variables["root"];
    let context = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);

    backend.fail_next.store(true, Ordering::SeqCst);
    assert!(matches!(
        vm.run(process, 1),
        Err(ousject_vm::VmError::Oms(OmsError::Storage(_)))
    ));
    assert_eq!(vm.process_state(process), Ok(before));
    assert_eq!(
        vm.manager().inspect(context, old_item).unwrap().lifecycle,
        LifecycleState::Active
    );
    assert_eq!(
        vm.manager().read(context, root).unwrap().links()["item"],
        old_item
    );

    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    let after = vm.process_state(process).unwrap();
    assert!(!after.variables.contains_key("alias"));
    assert!(!after.variables.contains_key("child"));
    assert_ne!(after.variables["item"], old_item);
    assert_eq!(
        vm.variable(process, "item"),
        Ok(Value::Text("fresh".to_owned()))
    );
    assert!(vm.manager().read(context, root).unwrap().links().is_empty());
    for retired in [old_item, old_child] {
        assert_eq!(
            vm.manager().inspect(context, retired).unwrap().lifecycle,
            LifecycleState::Tombstoned
        );
        assert!(matches!(
            vm.manager().value(context, retired),
            Err(OmsError::InvalidLifecycle { .. })
        ));
    }
}

#[test]
fn process_subject_is_persistent_and_enforced_for_object_access() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = VirtualMachine::new(manager.clone());
    let system = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);
    let secret = manager
        .create_object(
            system,
            CreateSpec::new("core.text", Value::Text("secret".to_owned())),
        )
        .unwrap();
    let subject = SubjectId::new();
    let program = compile(&format!(
        "secret = object.find(\"{secret}\")\ncopy = secret"
    ))
    .unwrap();
    let process = vm.create_process_as(&program, subject).unwrap();
    assert_eq!(vm.process_state(process).unwrap().subject, subject);

    assert!(matches!(
        vm.run(process, 100),
        Err(VmError::Oms(OmsError::Denied {
            capability: Capability::Inspect,
            ..
        }))
    ));

    let mut grant_inspect = manager.begin(system);
    grant_inspect
        .expect(secret, manager.inspect(system, secret).unwrap().version)
        .grant(secret, subject, Capability::Inspect);
    manager.commit(grant_inspect).unwrap();
    let process = vm.create_process_as(&program, subject).unwrap();
    assert!(matches!(
        vm.run(process, 100),
        Err(VmError::Oms(OmsError::Denied {
            capability: Capability::ViewValue,
            ..
        }))
    ));

    let mut grant_value = manager.begin(system);
    grant_value
        .expect(secret, manager.inspect(system, secret).unwrap().version)
        .grant(secret, subject, Capability::ViewValue);
    manager.commit(grant_value).unwrap();
    let process = vm.create_process_as(&program, subject).unwrap();
    assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.variable(process, "copy"),
        Ok(Value::Text("secret".to_owned()))
    );
}

#[test]
fn namespace_program_and_channel_capabilities_run_end_to_end() {
    let source = r#"
func receiver() {
    inbox = object.find("inbox")
    inbox.wait()
    message = inbox.receive()
    out = object.find("out")
    out.send(message)
}

root = object.create("core.namespace", {})
item = object.create("core.text", "named")
root.bind("item", item.id)
found = object.find(root.resolve("item"))
root.unbind("item")

inbox = object.create("core.channel", [])
out = object.create("core.channel", [])
child = object.create("core.process", {
    entry: "receiver",
    links: { inbox: inbox.id, out: out.id }
})
child.start()
first_status = child.wait()
inbox.send("hello")
second_status = child.wait()
received = out.receive()

program = object.find("program")
executed_id = program.execute("receiver")
executed = object.find(executed_id)
executed.terminate()
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = VirtualMachine::new(manager);
    let process = vm.create_process(&compile(source).unwrap()).unwrap();
    assert_eq!(
        vm.run(process, 1_000).unwrap().status,
        ProcessStatus::Halted
    );
    assert_eq!(
        vm.variable(process, "found"),
        Ok(Value::Text("named".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "first_status"),
        Ok(Value::Text("suspended".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "second_status"),
        Ok(Value::Text("halted".to_owned()))
    );
    assert_eq!(
        vm.variable(process, "received"),
        Ok(Value::Text("hello".to_owned()))
    );
    let root = vm.process_state(process).unwrap().variables["root"];
    assert!(
        vm.manager()
            .read(AccessContext::new(ousject_vm::SYSTEM_SUBJECT), root)
            .unwrap()
            .links()
            .is_empty()
    );
    let executed = vm.process_state(process).unwrap().variables["executed"];
    assert_eq!(
        vm.process_state(executed).unwrap().status,
        ProcessStatus::Terminated
    );
}

#[test]
fn shared_channel_atomically_wakes_a_waiter_owned_by_another_subject() {
    let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
    let vm = VirtualMachine::new(manager.clone());
    let system = AccessContext::new(ousject_vm::SYSTEM_SUBJECT);
    let channel = manager
        .create_object(
            system,
            CreateSpec::new("core.channel", Value::Array(Vec::new())),
        )
        .unwrap();
    let receiver_subject = SubjectId::new();
    let sender_subject = SubjectId::new();
    let mut grants = manager.begin(system);
    let channel_version = manager.inspect(system, channel).unwrap().version;
    grants.expect(channel, channel_version);
    for subject in [receiver_subject, sender_subject] {
        for capability in [
            Capability::Inspect,
            Capability::Invoke,
            Capability::ViewValue,
            Capability::ReplaceValue,
            Capability::Link,
        ] {
            grants.grant(channel, subject, capability);
        }
    }
    manager.commit(grants).unwrap();

    let receiver = vm
        .create_process_as(
            &compile(&format!(
                "channel = object.find(\"{channel}\")\nchannel.wait()\nmessage = channel.receive()"
            ))
            .unwrap(),
            receiver_subject,
        )
        .unwrap();
    let sender = vm
        .create_process_as(
            &compile(&format!(
                "channel = object.find(\"{channel}\")\nchannel.send(\"cross-user\")"
            ))
            .unwrap(),
            sender_subject,
        )
        .unwrap();

    assert_eq!(
        vm.run(receiver, 100).unwrap().status,
        ProcessStatus::Suspended
    );
    assert_eq!(vm.run(sender, 100).unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.process_state(receiver).unwrap().status,
        ProcessStatus::Running
    );
    assert_eq!(vm.run(receiver, 100).unwrap().status, ProcessStatus::Halted);
    assert_eq!(
        vm.variable(receiver, "message"),
        Ok(Value::Text("cross-user".to_owned()))
    );
}
