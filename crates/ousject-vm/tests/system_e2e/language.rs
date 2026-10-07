#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn extended_praxis_control_flow_runs_end_to_end() {
    let source = r#"
count = 0
sum = 0
terminal = object.find("terminal")

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
    terminal.println("short circuit failed")
} else if true || missing {
    terminal.println(sum)
    terminal.println(count)
}
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_terminal(manager);
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
terminal = object.find("terminal")
items[0] = 10
user = {
    name: "Ada",
    age: 18,
}
user["age"] = 19
terminal.println(items[0] + user["age"])
terminal.println(#items + #user + #"hi")
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_terminal(manager);
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

terminal = object.find("terminal")
counter = object.create("Counter", { value: 10 })
same link counter
terminal.println(counter.add(3))
terminal.println(same.value)
terminal.println(counter.reveal())
terminal.println(counter.type)
terminal.println(counter.inspect["type"])
terminal.println(#object.query("Counter", "add"))
terminal.println(#counter.capabilities)
"#;
    let directory = std::env::temp_dir().join(format!("ousject-class-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let process;
    let counter;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_terminal(manager);
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
    let vm = vm_with_terminal(manager);
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
terminal = object.find("terminal")
items = [1, 4]
items[0]++
items[1]--
terminal.println(items[0] + items[1])
terminal.println("你好，Ousject")

try {
    failed = 1 / 0
    terminal.println("not reached")
} catch (error) {
    terminal.println(error)
}

terminal.println("continued")
"#;
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_terminal(manager);
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
	terminal = object.find("terminal")
	item = object.create("core.text", "one")
	same = object.find(item.id)
	terminal.println(item.type)
	terminal.println(item.value)
	item.replace("two")
	root = object.create("core.namespace", {})
	child = object.create("core.text", "nested", root)
	parent = child.parent
	terminal.println(parent == root.id)
	terminal.println(#root.children)
	root.link("item", item)
	found = root.links["item"]
	found_object = object.find(found)
	terminal.println(found_object.value)
	terminal.println(#object.query("core.text"))
	terminal.println(item.inspect["type"])
	terminal.println(item.status)
	terminal.println(#item.capabilities)
	root.unlink("item")
	terminal.println(#root.links)
"#;
    let directory = std::env::temp_dir().join(format!("ousject-object-api-{}", ObjectId::new()));
    let path = directory.join("objects.oms");
    let process;
    let item;
    {
        let manager = Arc::new(InMemoryObjectManager::open_persistent(&path).unwrap());
        let vm = vm_with_terminal(manager);
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
                "18",
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
    let vm = vm_with_terminal(manager);
    assert_eq!(
        vm.variable(process, "item"),
        Ok(Value::Text("two".to_owned()))
    );
    assert_eq!(vm.process_state(process).unwrap().variables["item"], item);
    drop(vm);
    std::fs::remove_dir_all(directory).unwrap();
}
