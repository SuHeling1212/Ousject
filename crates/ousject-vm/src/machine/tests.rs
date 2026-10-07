#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn process_state_round_trip() {
        let state = ProcessState {
            program: ObjectId::new(),
            subject: SubjectId::new(),
            token_position: 42,
            stack: vec![Value::Integer(1), Value::Text("two".to_owned())],
            variables: BTreeMap::from([("answer".to_owned(), ObjectId::new())]),
            frames: Vec::new(),
            handlers: Vec::new(),
            status: ProcessStatus::Running,
            wait_reason: WaitReason::None,
            lease_owner: None,
            lease_generation: 0,
            lease_deadline_unix_ms: None,
            result: None,
            error: None,
            ended_at_unix_ms: None,
        };
        assert_eq!(
            decode_process_state(&encode_process_state(&state).unwrap()),
            Ok(state)
        );
    }

    #[test]
    fn praxis_can_index_binary_bytes_without_text_conversion() {
        let vm = vm_with_terminal(Arc::new(InMemoryObjectManager::new(1).unwrap()));
        let program = praxis_compiler::compile(
            "text = \"A\"\nbytes = text.utf8_bytes()\nfirst = bytes[0]\nlength = #bytes",
        )
        .unwrap();
        let process = vm.create_process(&program).unwrap();
        let report = vm.run(process, 100).unwrap_or_else(|error| {
            panic!("run failed: {error:?}; process={:?}", vm.process_state(process))
        });
        assert_eq!(report.status, ProcessStatus::Halted);
        assert_eq!(vm.variable(process, "first"), Ok(Value::Integer(65)));
        assert_eq!(vm.variable(process, "length"), Ok(Value::Integer(1)));
    }

    #[test]
    fn timer_sleep_suspends_without_blocking_the_process_worker() {
        let vm = vm_with_terminal(Arc::new(InMemoryObjectManager::new(1).unwrap()));
        let program = praxis_compiler::compile(
            "time = object.find(\"time\")\ntime.sleep(10000)\nanswer = 42",
        )
        .unwrap();
        let process = vm.create_process(&program).unwrap();
        let report = vm.run(process, 1_000).unwrap();
        assert_eq!(report.status, ProcessStatus::Waiting);
        let state = vm.process_state(process).unwrap();
        assert!(timer_deadline(&state.wait_reason).is_some());
        assert_eq!(state.status, ProcessStatus::Waiting);
        assert!(!vm.wake_due_timer(process).unwrap());
    }

    #[test]
    fn scheduler_runs_other_processes_before_waking_a_timer_sleeper() {
        let vm = vm_with_terminal(Arc::new(InMemoryObjectManager::new(1).unwrap()));
        let sleeper = vm
            .create_process(
                &praxis_compiler::compile(
                    "time = object.find(\"time\")\ntime.sleep(20)\nfinished = true",
                )
                .unwrap(),
            )
            .unwrap();
        let worker = vm
            .create_process(&praxis_compiler::compile("finished = true").unwrap())
            .unwrap();
        let mut scheduler = CooperativeScheduler::new(&vm);
        scheduler.enqueue(sleeper);
        scheduler.enqueue(worker);
        let report = scheduler.run(1000).unwrap();
        assert_eq!(report.processes.len(), 2);
        assert!(
            report
                .processes
                .iter()
                .all(|process| process.status == ProcessStatus::Halted)
        );
    }

    #[test]
    fn expired_finished_process_tree_is_retired_but_program_is_not() {
        let manager = Arc::new(InMemoryObjectManager::new(2).unwrap());
        let vm = vm_with_terminal(Arc::clone(&manager));
        let program = praxis_compiler::compile("answer = 42").unwrap();
        let process = vm.create_process(&program).unwrap();
        assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
        let mut state = vm.process_state(process).unwrap();
        state.ended_at_unix_ms = Some(0);
        let view = manager
            .read(AccessContext::new(SYSTEM_SUBJECT), process)
            .unwrap();
        let mut transaction = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state).unwrap());
        manager.commit(transaction).unwrap();

        reap_expired_processes(
            &manager,
            UNIX_EPOCH + Duration::from_millis(PROCESS_RETENTION_MILLIS + 10),
        )
        .unwrap();

        let context = AccessContext::new(SYSTEM_SUBJECT);
        assert_eq!(
            manager.inspect(context, process).unwrap().lifecycle,
            LifecycleState::Tombstoned
        );
        assert_eq!(
            manager
                .inspect(context, state.variables["answer"])
                .unwrap()
                .lifecycle,
            LifecycleState::Tombstoned
        );
        assert_eq!(
            manager.inspect(context, state.program).unwrap().lifecycle,
            LifecycleState::Active
        );
    }

    #[test]
    fn process_reaper_wakes_for_a_newly_ended_process() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = vm_with_terminal(Arc::clone(&manager));
        let process = vm
            .create_process(&praxis_compiler::compile("answer = 42").unwrap())
            .unwrap();
        assert_eq!(vm.run(process, 100).unwrap().status, ProcessStatus::Halted);
        let reaper = ProcessReaper::start(&vm, Duration::from_secs(1)).unwrap();

        let mut state = vm.process_state(process).unwrap();
        state.ended_at_unix_ms = Some(0);
        let view = manager
            .read(AccessContext::new(SYSTEM_SUBJECT), process)
            .unwrap();
        let mut transaction = manager.begin(AccessContext::new(SYSTEM_SUBJECT));
        transaction
            .expect(process, view.header().version)
            .update_state(process, encode_process_state(&state).unwrap());
        manager.commit(transaction).unwrap();
        vm.notify_process_ended(process).unwrap();

        let context = AccessContext::new(SYSTEM_SUBJECT);
        for _ in 0..100 {
            if manager.inspect(context, process).unwrap().lifecycle == LifecycleState::Tombstoned {
                drop(reaper);
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        drop(reaper);
        panic!("the background Process reaper did not retire the expired Process");
    }

    #[test]
    fn rejects_old_process_versions() {
        assert!(matches!(
            decode_process_state(b"OPS1"),
            Err(VmError::InvalidProcessState(_))
        ));
    }

    #[test]
    fn executes_program_with_variable_objects() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = vm_with_terminal(manager);
        let program = praxis_compiler::compile(
            "terminal = object.find(\"terminal\")\nanswer = 40 + 2\nterminal.println(answer)",
        )
        .unwrap();
        let process = vm.create_process(&program).unwrap();
        let report = vm.run(process, 100).unwrap();
        assert_eq!(report.status, ProcessStatus::Halted);
        assert_eq!(report.output, vec!["42"]);
        assert_eq!(vm.variable(process, "answer"), Ok(Value::Integer(42)));
        assert_eq!(vm.process_state(process).unwrap().variables.len(), 2);
        vm.manager().health_check().unwrap();
    }

    #[test]
    fn terminal_is_shared_and_requires_a_provider_for_this_boot() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = vm_with_terminal(manager.clone());
        let program = praxis_compiler::compile(
            "terminal = object.find(\"terminal\")\nterminal.println(\"ready\")",
        )
        .unwrap();
        let first = vm.create_process(&program).unwrap();
        let second = vm.create_process(&program).unwrap();
        let context = AccessContext::new(SYSTEM_SUBJECT);
        let first_terminal = manager.read(context, first).unwrap().links()["terminal"];
        let second_terminal = manager.read(context, second).unwrap().links()["terminal"];
        assert_eq!(first_terminal, second_terminal);

        assert_eq!(vm.run(first, 2).unwrap().status, ProcessStatus::Ready);
        let disconnected = VirtualMachine::new(manager);
        assert_eq!(
            disconnected.run(first, 10),
            Err(VmError::MissingProvider("terminal"))
        );
    }

    #[test]
    fn step_limit_preserves_resumable_position() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = VirtualMachine::new(manager);
        let program = Program {
            tokens: vec![Token::Jump(0), Token::Halt],
        };
        let process = vm.create_process(&program).unwrap();
        assert_eq!(vm.run(process, 5).unwrap().status, ProcessStatus::Ready);
        assert_eq!(vm.process_state(process).unwrap().token_position, 0);
    }

    #[test]
    fn scheduler_runs_processes_round_robin() {
        let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
        let vm = vm_with_terminal(manager);
        let first = vm
            .create_process(
                &praxis_compiler::compile(
                    "terminal = object.find(\"terminal\")\nterminal.println(\"a\")",
                )
                .unwrap(),
            )
            .unwrap();
        let second = vm
            .create_process(
                &praxis_compiler::compile(
                    "terminal = object.find(\"terminal\")\nterminal.println(\"b\")",
                )
                .unwrap(),
            )
            .unwrap();
        let mut scheduler = CooperativeScheduler::new(&vm);
        scheduler.enqueue(first);
        scheduler.enqueue(second);
        let report = scheduler.run(20).unwrap();
        assert_eq!(report.processes.len(), 2);
        assert!(
            report
                .processes
                .iter()
                .all(|process| process.status == ProcessStatus::Halted)
        );
        assert_eq!(report.total_steps, 14);
    }

    #[test]
    fn persistent_process_resumes_after_reopen() {
        let directory = std::env::temp_dir().join(format!("ousject-vm-{}", ObjectId::new()));
        let path = directory.join("objects.oms");
        let program = praxis_program_for_recovery();

        let process = {
            let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
            let vm = vm_with_terminal(manager);
            let process = vm.create_process(&program).unwrap();
            assert_eq!(vm.run(process, 5).unwrap().status, ProcessStatus::Ready);
            process
        };

        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_terminal(manager);
        vm.reconnect_hardware(process).unwrap();
        let report = vm.run(process, 100).unwrap();
        assert_eq!(report.output, vec!["3"]);
        assert_eq!(vm.variable(process, "count"), Ok(Value::Integer(3)));
        drop(vm);
        std::fs::remove_dir_all(directory).unwrap();
    }

    fn praxis_program_for_recovery() -> Program {
        praxis_compiler::compile(
            "terminal = object.find(\"terminal\")\ncount = 0\nwhile count < 3 { count++ }\nterminal.println(count)",
        )
        .unwrap()
    }
}
