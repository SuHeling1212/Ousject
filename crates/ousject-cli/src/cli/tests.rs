use super::*;

fn detached_terminal_input() -> (LinuxConsole, mpsc::Receiver<()>) {
    let (start_reader, receiver) = mpsc::channel();
    (
        LinuxConsole {
            input: Arc::new(Mutex::new(LinuxInputState {
                owner: None,
                line: String::new(),
                completed_lines: BTreeMap::new(),
                events: BTreeMap::new(),
                overflowed: BTreeSet::new(),
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
fn console_lines_and_keyboard_capture_share_exclusive_process_lease() {
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
fn host_network_provider_connects_sends_receives_and_closes() {
    let server = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let address = server.local_addr().unwrap();
    let worker = std::thread::spawn(move || {
        let (mut stream, _) = server.accept().unwrap();
        let mut bytes = [0_u8; 5];
        stream.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"hello");
        stream.write_all(b"world").unwrap();
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
                "send",
                &[Value::Text("hello".to_owned())],
                ObjectId::new(),
            )
            .unwrap()
            .result,
        Value::Integer(5)
    );
    assert_eq!(
        provider
            .invoke(object, &state, "receive", &[], ObjectId::new())
            .unwrap()
            .result,
        Value::Bytes(b"world".to_vec())
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

    drop(provider);
    std::fs::remove_dir_all(directory).unwrap();
}
