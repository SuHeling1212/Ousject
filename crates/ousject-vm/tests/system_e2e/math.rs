#![allow(clippy::wildcard_imports)]

use super::*;

#[test]
fn praxis_uses_math_object_capabilities_and_constants() {
    let manager = Arc::new(InMemoryObjectManager::new(1).unwrap());
    let vm = vm_with_terminal(manager);
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
    let vm = vm_with_terminal(manager);
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
