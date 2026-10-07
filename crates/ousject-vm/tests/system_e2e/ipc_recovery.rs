#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn channel_send_and_consume_survive_store_restarts_exactly_once() {
    let directory =
        std::env::temp_dir().join(format!("ousject-channel-recovery-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let channel = {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_terminal(manager);
        let program = compile(
            "channel = object.create(\"core.channel\", [])\nchannel.send(\"durable message\")\nchannel_id = channel.id",
        )
        .unwrap();
        let process = vm.create_process(&program).unwrap();
        assert_eq!(
            vm.run(process, 1_000).unwrap().status,
            ProcessStatus::Halted
        );
        let Value::Text(channel) = vm.variable(process, "channel_id").unwrap() else {
            panic!("expected Channel Object id");
        };
        channel.parse::<ObjectId>().unwrap()
    };

    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        assert_eq!(
            manager.value(AccessContext::new(SYSTEM_SUBJECT), channel),
            Ok(Value::Array(vec![Value::Text(
                "durable message".to_owned()
            )]))
        );
        let vm = vm_with_terminal(manager.clone());
        let program = compile(&format!(
            "channel = object.find(\"{channel}\")\nreceived = channel.receive()"
        ))
        .unwrap();
        let process = vm.create_process(&program).unwrap();
        assert_eq!(
            vm.run(process, 1_000).unwrap().status,
            ProcessStatus::Halted
        );
        assert_eq!(
            vm.variable(process, "received"),
            Ok(Value::Text("durable message".to_owned()))
        );
        assert_eq!(
            manager.value(AccessContext::new(SYSTEM_SUBJECT), channel),
            Ok(Value::Array(Vec::new()))
        );
    }

    let recovered = InMemoryObjectManager::open_persistent(&path).unwrap();
    assert_eq!(
        recovered.value(AccessContext::new(SYSTEM_SUBJECT), channel),
        Ok(Value::Array(Vec::new()))
    );
    drop(recovered);
    std::fs::remove_dir_all(directory).unwrap();
}
