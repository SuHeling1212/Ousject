use super::*;

fn detached_terminal_input() -> (LinuxTerminal, mpsc::Receiver<()>) {
    let (start_reader, receiver) = mpsc::channel();
    (
        LinuxTerminal {
            input: Arc::new(Mutex::new(LinuxInputState {
                owner: None,
                line: String::new(),
                completed_lines: BTreeMap::new(),
                events: BTreeMap::new(),
                overflowed: BTreeSet::new(),
                raw_bytes: BTreeMap::new(),
                raw_overflowed: BTreeSet::new(),
                interrupt_watchers: BTreeSet::new(),
                interrupted: BTreeSet::new(),
                unclaimed: VecDeque::new(),
                eof: false,
                reader_error: None,
            })),
            running: Arc::new(AtomicBool::new(false)),
            start_reader,
            reader: Mutex::new(None),
            original_termios: Mutex::new(None),
            terminal: false,
        },
        receiver,
    )
}

#[test]
fn terminal_lines_and_keyboard_capture_share_exclusive_process_lease() {
    let (terminal, _reader) = detached_terminal_input();
    let line_process = ObjectId::new();
    let keyboard_process = ObjectId::new();
    assert_eq!(terminal.poll_line(line_process, InputMode::Line), Ok(None));
    assert_eq!(
        terminal.capture_keyboard(keyboard_process),
        Err(ProviderError::Pending)
    );

    terminal.release_process_input(line_process);
    terminal.capture_keyboard(keyboard_process).unwrap();
    assert_eq!(terminal.poll_line(line_process, InputMode::Line), Ok(None));
    terminal.release_process_input(keyboard_process);
    assert_eq!(
        terminal.input.lock().unwrap().owner,
        None,
        "a dead Process must release terminal input"
    );
}

#[test]
fn ctrl_c_is_reported_to_the_watcher_and_as_a_line_cancel_marker() {
    let (terminal, _reader) = detached_terminal_input();
    let process = ObjectId::new();
    terminal.poll_line(process, InputMode::Line).unwrap();
    terminal
        .input
        .lock()
        .unwrap()
        .interrupt_watchers
        .insert(process);
    dispatch_event(
        &terminal.input,
        KeyEvent {
            key: "c".to_owned(),
            text: None,
            control: true,
            alt: false,
            shift: false,
        },
        false,
    );

    assert!(terminal.take_interrupt(process).unwrap());
    assert_eq!(
        terminal.poll_line(process, InputMode::Line).unwrap(),
        Some("\r".to_owned())
    );
}

#[test]
fn terminal_parser_decodes_text_arrows_modifiers_and_function_keys() {
    let mut parser = InputParser::default();
    let mut events = Vec::new();
    for byte in "你".as_bytes() {
        events.extend(parser.push(*byte));
    }
    assert_eq!(events[0].text.as_deref(), Some("你"));

    let mut events = Vec::new();
    for byte in b"\x1b[1;5A" {
        events.extend(parser.push(*byte));
    }
    assert_eq!(events[0].key, "arrow_up");
    assert!(events[0].control);

    let mut events = Vec::new();
    for byte in b"\x1b[24~" {
        events.extend(parser.push(*byte));
    }
    assert_eq!(events[0].key, "f12");

    for (sequence, expected) in [
        (&b"\x1b[H"[..], "home"),
        (&b"\x1b[F"[..], "end"),
        (&b"\x1b[2~"[..], "insert"),
        (&b"\x1b[3~"[..], "delete"),
        (&b"\x1b[5~"[..], "page_up"),
        (&b"\x1b[6~"[..], "page_down"),
    ] {
        let mut parser = InputParser::default();
        let decoded = sequence
            .iter()
            .flat_map(|byte| parser.push(*byte))
            .next()
            .unwrap();
        assert_eq!(decoded.key, expected);
    }

    let mut parser = InputParser::default();
    let alt = b"\x1bq"
        .iter()
        .flat_map(|byte| parser.push(*byte))
        .next()
        .unwrap();
    assert_eq!(alt.text.as_deref(), Some("q"));
    assert!(alt.alt);

    let control_z = parser.push(0x1a).remove(0);
    assert_eq!(control_z.key, "z");
    assert!(control_z.control);
}

#[test]
fn standalone_escape_is_reported_after_the_sequence_timeout() {
    let mut parser = InputParser::default();
    assert!(parser.push(0x1b).is_empty());
    assert_eq!(
        parser
            .flush_escape_timeout(std::time::Instant::now() + Duration::from_secs(1))
            .unwrap()
            .key,
        "escape"
    );
}

#[test]
fn terminal_input_uses_a_binary_exclusive_lease_and_canonical_lines() {
    let (terminal, _reader) = detached_terminal_input();
    let raw_process = ObjectId::new();
    assert_eq!(
        terminal.poll_terminal_bytes(raw_process, 8).unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(
        terminal.input.lock().unwrap().owner,
        Some((raw_process, InputMode::TerminalRaw))
    );
    assert_eq!(
        terminal.poll_terminal_bytes(ObjectId::new(), 8).unwrap(),
        Vec::<u8>::new()
    );
    terminal
        .input
        .lock()
        .unwrap()
        .raw_bytes
        .entry(raw_process)
        .or_default()
        .extend([0x1b, b'[', b'A']);
    assert_eq!(
        terminal.poll_terminal_bytes(raw_process, 2).unwrap(),
        [0x1b, b'[']
    );
    assert_eq!(
        terminal.poll_terminal_bytes(raw_process, 8).unwrap(),
        [b'A']
    );
    assert_eq!(
        terminal.capture_keyboard(ObjectId::new()),
        Err(ProviderError::Pending)
    );
    terminal.release_process_input(raw_process);

    let line_process = ObjectId::new();
    assert_eq!(terminal.poll_terminal_line(line_process, false), Ok(None));
    for character in ['o', 'k'] {
        dispatch_event(
            &terminal.input,
            KeyEvent {
                key: character.to_string(),
                text: Some(character.to_string()),
                control: false,
                alt: false,
                shift: false,
            },
            false,
        );
    }
    dispatch_event(
        &terminal.input,
        KeyEvent {
            key: "enter".to_owned(),
            text: None,
            control: false,
            alt: false,
            shift: false,
        },
        false,
    );
    assert_eq!(
        terminal.poll_terminal_line(line_process, false),
        Ok(Some("ok".to_owned()))
    );
}

#[test]
fn terminal_provider_keeps_child_screens_separate_and_supports_resize() {
    let (terminal, _reader) = detached_terminal_input();
    let root = ObjectId::new();
    let provider = HostTerminalProvider::new(root, Arc::new(terminal));
    let root_state = provider.create(&Value::Null).unwrap();
    let created = provider
        .invoke(root, &root_state, "create", &[], ObjectId::new())
        .unwrap();
    let Value::Text(child_text) = created.result else {
        panic!("create returns the child Object ID");
    };
    let child: ObjectId = child_text.parse().unwrap();
    assert_eq!(created.created[0].parent, Some(root));

    let resized = provider
        .invoke(
            child,
            &provider.create(&Value::Null).unwrap(),
            "resize",
            &[Value::Integer(12), Value::Integer(4)],
            ObjectId::new(),
        )
        .unwrap();
    assert_eq!(
        resized.object_state.as_ref().unwrap(),
        &Value::Record(BTreeMap::from([
            ("columns".to_owned(), Value::Integer(12)),
            ("echo".to_owned(), Value::Bool(false)),
            ("foreground_process".to_owned(), Value::Null),
            ("input_mode".to_owned(), Value::Text("raw".to_owned())),
            ("parent_terminal".to_owned(), Value::Null),
            ("rows".to_owned(), Value::Integer(4)),
        ]))
    );
    let state = resized.object_state.unwrap();
    provider
        .invoke_ephemeral_for_process(
            ObjectId::new(),
            child,
            &state,
            "output",
            &[Value::Bytes(b"\x1b[2;3Hhi".to_vec())],
        )
        .unwrap();
    let snapshot = provider
        .invoke_ephemeral_for_process(ObjectId::new(), child, &state, "snapshot", &[])
        .unwrap();
    let Value::Record(fields) = snapshot.result else {
        panic!("Terminal snapshot returns a Record");
    };
    assert_eq!(fields.get("columns"), Some(&Value::Integer(12)));
    assert_eq!(fields.get("rows"), Some(&Value::Integer(4)));
    assert_eq!(
        fields.get("lines"),
        Some(&Value::Array(vec![
            Value::Text(String::new()),
            Value::Text("  hi".to_owned()),
            Value::Text(String::new()),
            Value::Text(String::new()),
        ]))
    );
}

#[test]
fn child_terminal_foreground_routes_input_and_restores_its_parent() {
    let (terminal, _reader) = detached_terminal_input();
    let terminal = Arc::new(terminal);
    let root = ObjectId::new();
    let provider = HostTerminalProvider::new(root, Arc::clone(&terminal));
    let root_state = provider.create(&Value::Null).unwrap();
    let created = provider
        .invoke(root, &root_state, "create", &[], ObjectId::new())
        .unwrap();
    let child = created.created[0].id;
    let child_state = Value::decode(&created.created[0].state).unwrap();
    let process = ObjectId::new();
    let foreground = provider
        .invoke_for_process(
            process,
            child,
            &child_state,
            "foreground",
            &[],
            ObjectId::new(),
        )
        .unwrap();
    let child_state = foreground.object_state.unwrap();
    let mut input_bytes = (0_u8..=u8::MAX).collect::<Vec<_>>();
    input_bytes.extend_from_slice(
        b"\r\t\x7f\x1b[A\x1b[B\x1b[C\x1b[D\x1bOA\x1bOB\x1bOC\x1bOD\x1b[H\x1b[F\x1b[2~\x1b[3~\x1b[5~\x1b[6~\x1bOP\x1bOQ\x1bOR\x1bOS\x1b[15~\x1b[17~\x1b[18~\x1b[19~\x1b[20~\x1b[21~\x1b[23~\x1b[24~\x1ba\x01\x1a\x1b[200~paste \xf0\x9f\x90\x8d\x80\xff\x1b[201~\x1b[<0;12;5M\x1b[<0;12;5m",
    );
    terminal
        .input
        .lock()
        .unwrap()
        .raw_bytes
        .entry(process)
        .or_default()
        .extend(input_bytes.iter().copied());
    let input = provider
        .invoke_ephemeral_for_process(
            process,
            child,
            &child_state,
            "input",
            &[Value::Integer(512)],
        )
        .unwrap();
    assert_eq!(input.result, Value::Bytes(input_bytes));

    provider.process_ended(process);
    assert_eq!(
        provider.active_terminal_id().unwrap(),
        root,
        "an ended child foreground must restore the parent Terminal"
    );
}

#[test]
fn nested_child_terminals_restore_each_parent_after_process_exit() {
    let (terminal, _reader) = detached_terminal_input();
    let root = ObjectId::new();
    let provider = HostTerminalProvider::new(root, Arc::new(terminal));
    let root_state = provider.create(&Value::Null).unwrap();
    let child_created = provider
        .invoke(root, &root_state, "create", &[], ObjectId::new())
        .unwrap();
    let child = child_created.created[0].id;
    let child_state = Value::decode(&child_created.created[0].state).unwrap();
    let nested_created = provider
        .invoke(child, &child_state, "create", &[], ObjectId::new())
        .unwrap();
    let nested = nested_created.created[0].id;
    let nested_state = Value::decode(&nested_created.created[0].state).unwrap();
    let child_process = ObjectId::new();
    let nested_process = ObjectId::new();

    provider
        .invoke_for_process(
            child_process,
            child,
            &child_state,
            "foreground",
            &[],
            ObjectId::new(),
        )
        .unwrap();
    provider
        .invoke_for_process(
            nested_process,
            nested,
            &nested_state,
            "foreground",
            &[],
            ObjectId::new(),
        )
        .unwrap();
    assert_eq!(provider.active_terminal_id().unwrap(), nested);

    // A failed/terminated nested Process releases its input lease and returns
    // control to the exact parent Terminal, not directly to the root.
    provider.process_ended(nested_process);
    assert_eq!(provider.active_terminal_id().unwrap(), child);
    provider.process_ended(child_process);
    assert_eq!(provider.active_terminal_id().unwrap(), root);
}

#[test]
fn retiring_a_foreground_child_terminal_releases_its_input_owner() {
    let (terminal, _reader) = detached_terminal_input();
    let terminal = Arc::new(terminal);
    let root = ObjectId::new();
    let provider = HostTerminalProvider::new(root, Arc::clone(&terminal));
    let root_state = provider.create(&Value::Null).unwrap();
    let created = provider
        .invoke(root, &root_state, "create", &[], ObjectId::new())
        .unwrap();
    let child = created.created[0].id;
    let child_state = Value::decode(&created.created[0].state).unwrap();
    let process = ObjectId::new();
    provider
        .invoke_for_process(
            process,
            child,
            &child_state,
            "foreground",
            &[],
            ObjectId::new(),
        )
        .unwrap();
    provider
        .invoke_ephemeral_for_process(process, child, &child_state, "input", &[Value::Integer(8)])
        .unwrap();
    assert_eq!(provider.active_terminal_id().unwrap(), child);
    assert_eq!(
        terminal.input.lock().unwrap().owner,
        Some((process, InputMode::TerminalRaw))
    );

    provider.object_retired(child);

    assert_eq!(provider.active_terminal_id().unwrap(), root);
    assert_eq!(terminal.input.lock().unwrap().owner, None);
    let next_process = ObjectId::new();
    assert_eq!(
        terminal.poll_terminal_bytes(next_process, 8).unwrap(),
        Vec::<u8>::new()
    );
    assert_eq!(
        terminal.input.lock().unwrap().owner,
        Some((next_process, InputMode::TerminalRaw))
    );
}

#[test]
fn terminal_resize_is_delivered_as_a_waitable_event() {
    let (terminal, _reader) = detached_terminal_input();
    let root = ObjectId::new();
    let provider = HostTerminalProvider::new(root, Arc::new(terminal));
    let state = provider.create(&Value::Null).unwrap();
    let process = ObjectId::new();
    assert!(matches!(
        provider.invoke_for_process(process, root, &state, "wait_event", &[], ObjectId::new(),),
        Err(ProviderError::Pending)
    ));
    let resized = provider
        .invoke(
            root,
            &state,
            "resize",
            &[Value::Integer(96), Value::Integer(32)],
            ObjectId::new(),
        )
        .unwrap();
    let event = provider
        .invoke_for_process(
            process,
            root,
            resized.object_state.as_ref().unwrap(),
            "wait_event",
            &[],
            ObjectId::new(),
        )
        .unwrap();
    assert_eq!(
        event.result,
        Value::Record(BTreeMap::from([
            ("kind".to_owned(), Value::Text("resize".to_owned())),
            ("columns".to_owned(), Value::Integer(96)),
            ("rows".to_owned(), Value::Integer(32)),
        ]))
    );
}

#[test]
fn terminal_foreground_hierarchy_recovers_and_clears_after_process_exit() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let system = AccessContext::new(SYSTEM_SUBJECT);
    let root_state = Value::Record(BTreeMap::from([
        ("columns".to_owned(), Value::Integer(80)),
        ("rows".to_owned(), Value::Integer(24)),
        ("input_mode".to_owned(), Value::Text("raw".to_owned())),
        ("echo".to_owned(), Value::Bool(false)),
        ("parent_terminal".to_owned(), Value::Null),
        ("foreground_process".to_owned(), Value::Null),
    ]));
    let root = ousject_vm::VirtualMachine::publish_provider_object(
        &manager,
        CORE_TERMINAL_TYPE,
        &root_state,
    )
    .unwrap();
    let child = ObjectId::new();
    let process = ObjectId::new();
    let child_state = Value::Record(BTreeMap::from([
        ("columns".to_owned(), Value::Integer(100)),
        ("rows".to_owned(), Value::Integer(30)),
        ("input_mode".to_owned(), Value::Text("raw".to_owned())),
        ("echo".to_owned(), Value::Bool(false)),
        ("parent_terminal".to_owned(), Value::Text(root.to_string())),
        (
            "foreground_process".to_owned(),
            Value::Text(process.to_string()),
        ),
    ]));
    let root_version = manager.inspect(system, root).unwrap().version;
    let mut transaction = manager.begin(system);
    transaction.expect(root, root_version).create(
        CreateObject::new(CORE_TERMINAL_TYPE, child_state.encode().unwrap())
            .with_id(child)
            .with_parent(root),
    );
    manager.commit(transaction).unwrap();

    let (terminal, _reader) = detached_terminal_input();
    let provider = HostTerminalProvider::with_manager(root, Arc::new(terminal), manager.clone());
    assert_eq!(provider.active_terminal_id().unwrap(), child);
    provider.process_ended(process);
    assert_eq!(provider.active_terminal_id().unwrap(), root);
    let Value::Record(state) = manager.value(system, child).unwrap() else {
        panic!("expected persisted Terminal state");
    };
    assert_eq!(state.get("foreground_process"), Some(&Value::Null));
}

#[test]
fn terminal_recovery_detaches_a_previously_ended_foreground_process() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let terminal =
        VirtualMachine::publish_terminal(&manager, &Value::Record(BTreeMap::new())).unwrap();
    let system = AccessContext::new(SYSTEM_SUBJECT);
    let (root, ended_process) = {
        let (terminal_driver, _terminal_reader) = detached_terminal_input();
        let vm =
            VirtualMachine::with_terminal(manager.clone(), terminal, Arc::new(terminal_driver))
                .unwrap();
        let root = manager
            .query(system, &ObjectQuery::new().with_type(CORE_TERMINAL_TYPE))
            .unwrap()
            .into_iter()
            .find(|terminal| terminal.parent_id.is_none())
            .unwrap()
            .id;
        let process = vm
            .create_process(&Program {
                tokens: vec![tf_format::Token::Halt],
            })
            .unwrap();
        assert_eq!(vm.run(process, 10).unwrap().status, ProcessStatus::Halted);
        (root, process)
    };

    let child = ObjectId::new();
    let child_state = Value::Record(BTreeMap::from([
        ("columns".to_owned(), Value::Integer(80)),
        ("rows".to_owned(), Value::Integer(24)),
        ("input_mode".to_owned(), Value::Text("raw".to_owned())),
        ("echo".to_owned(), Value::Bool(false)),
        ("parent_terminal".to_owned(), Value::Text(root.to_string())),
        (
            "foreground_process".to_owned(),
            Value::Text(ended_process.to_string()),
        ),
    ]));
    let mut child_request = CreateObject::new(CORE_TERMINAL_TYPE, child_state.encode().unwrap())
        .with_id(child)
        .with_parent(root);
    child_request.capabilities = manager.type_by_id(CORE_TERMINAL_TYPE).unwrap().capabilities;
    let mut transaction = manager.begin(system);
    transaction
        .expect(root, manager.inspect(system, root).unwrap().version)
        .create(child_request);
    manager.commit(transaction).unwrap();

    let (terminal, _reader) = detached_terminal_input();
    let provider = Arc::new(HostTerminalProvider::with_manager(
        root,
        Arc::new(terminal),
        manager.clone(),
    ));
    assert_eq!(provider.active_terminal_id().unwrap(), child);

    let (terminal_driver, _terminal_reader) = detached_terminal_input();
    let vm =
        VirtualMachine::with_terminal_backend(manager.clone(), root, Arc::new(terminal_driver))
            .unwrap();
    vm.register_provider(provider.clone()).unwrap();
    vm.recover_processes().unwrap();

    assert_eq!(provider.active_terminal_id().unwrap(), root);
    let Value::Record(fields) = manager.value(system, child).unwrap() else {
        panic!("expected persisted child Terminal state");
    };
    assert_eq!(fields.get("foreground_process"), Some(&Value::Null));
}

#[test]
fn host_network_provider_connects_sends_receives_and_closes() {
    let payload = (0_u8..=u8::MAX).collect::<Vec<_>>();
    let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = server.local_addr().unwrap();
    let expected = payload.clone();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = server.accept().unwrap();
        let mut bytes = vec![0_u8; expected.len()];
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(bytes, expected);
        stream.write_all(&bytes).unwrap();
    });

    let provider = HostNetworkProvider::default();
    let object = ObjectId::new();
    let mut state = provider
        .create(&Value::Map(BTreeMap::from([(
            "transport".to_owned(),
            Value::Text("tcp".to_owned()),
        )])))
        .unwrap();
    let connected = provider
        .invoke(
            object,
            &state,
            "connect",
            &[
                Value::Text("127.0.0.1".to_owned()),
                Value::Integer(i64::from(address.port())),
            ],
            ObjectId::new(),
        )
        .unwrap();
    state = connected.object_state.unwrap();
    assert_eq!(
        provider
            .invoke(
                object,
                &state,
                "output",
                &[Value::Bytes(payload.clone())],
                ObjectId::new(),
            )
            .unwrap()
            .result,
        Value::Integer(256)
    );
    assert_eq!(
        provider
            .invoke(
                object,
                &state,
                "input",
                &[Value::Integer(256)],
                ObjectId::new(),
            )
            .unwrap()
            .result,
        Value::Bytes(payload)
    );
    provider
        .invoke(object, &state, "close", &[], ObjectId::new())
        .unwrap();
    worker.join().unwrap();
}

#[test]
fn host_block_storage_provider_round_trips_a_synced_block() {
    let directory = std::env::temp_dir().join(format!("ousject-block-test-{}", ObjectId::new()));
    let path = directory.join("blocks.bin");
    let provider = HostBlockStorageProvider::open(path).unwrap();
    let object = ObjectId::new();
    let bytes = vec![7_u8; 4096];

    assert_eq!(
        provider
            .invoke(
                object,
                &Value::Null,
                "store_block",
                &[Value::Integer(2), Value::Bytes(bytes.clone())],
                ObjectId::new(),
            )
            .unwrap()
            .result,
        Value::Integer(4096)
    );
    assert_eq!(
        provider
            .invoke(
                object,
                &Value::Null,
                "load_block",
                &[Value::Integer(2)],
                ObjectId::new(),
            )
            .unwrap()
            .result,
        Value::Bytes(bytes)
    );

    let binary = (0_u8..=u8::MAX).collect::<Vec<_>>();
    let sequential = Value::Record(BTreeMap::from([("offset".to_owned(), Value::Integer(0))]));
    let written = provider
        .invoke(
            object,
            &sequential,
            "output",
            &[Value::Bytes(binary.clone())],
            ObjectId::new(),
        )
        .unwrap();
    assert_eq!(written.result, Value::Integer(256));
    assert_eq!(
        provider
            .invoke(
                object,
                &sequential,
                "input",
                &[Value::Integer(256)],
                ObjectId::new(),
            )
            .unwrap()
            .result,
        Value::Bytes(binary)
    );

    drop(provider);
    std::fs::remove_dir_all(directory).unwrap();
}
