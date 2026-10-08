use super::*;
use alloc::vec;
use tf_format::{Token, Value};

#[test]
fn compiles_variables_and_terminal_capability() {
    let program = compile(
        "terminal = object.find(\"terminal\")\nanswer = 40 + 2\nterminal.println(answer)\n",
    )
    .unwrap();
    assert_eq!(
        program.tokens,
        vec![
            Token::Push(Value::Text("terminal".to_owned())),
            Token::BindFound {
                name: "terminal".to_owned(),
                arguments: 1,
            },
            Token::Push(Value::Integer(40)),
            Token::Push(Value::Integer(2)),
            Token::Add,
            Token::Store("answer".to_owned()),
            Token::LoadIdentity("terminal".to_owned()),
            Token::Load("answer".to_owned()),
            Token::ObjectCall {
                method: "println".to_owned(),
                arguments: 1,
            },
            Token::Pop,
            Token::Halt,
        ]
    );
}

#[test]
fn compiles_explicit_object_registry_calls() {
    let program =
        compile("item = object.create(\"core.text\", \"one\")\nitem.replace(\"two\")\n").unwrap();
    assert!(program.tokens.contains(&Token::BindCreated {
        name: "item".to_owned(),
        arguments: 2,
    }));
    assert!(program.tokens.contains(&Token::ObjectCall {
        method: "replace".to_owned(),
        arguments: 1,
    }));
    assert!(compile("objects.create(\"core.text\", \"one\")").is_err());
    assert!(compile("io.println(\"old\")").is_err());
    let discovered = compile("aaa = object.find(\"terminal\")\naaa.println(\"hello\")").unwrap();
    assert!(discovered.tokens.contains(&Token::BindFound {
        name: "aaa".to_owned(),
        arguments: 1,
    }));
    assert!(discovered.tokens.contains(&Token::ObjectCall {
        method: "println".to_owned(),
        arguments: 1,
    }));
    assert!(compile("object.call(\"terminal\", \"print\")").is_err());
    assert!(compile("object.terminal()").is_err());
    assert!(compile("missing_call()").is_ok());
}

#[test]
fn compiles_loop_and_condition() {
    let source = r#"
count = 0
terminal = object.find("terminal")
while count < 3 {
    count++
}
if count == 3 {
    terminal.println("ok")
} else {
    terminal.println("bad")
}
"#;
    let program = compile(source).unwrap();
    assert!(
        program
            .tokens
            .iter()
            .any(|token| matches!(token, Token::Jump(_)))
    );
    assert!(
        program
            .tokens
            .iter()
            .any(|token| matches!(token, Token::JumpIfFalse(_)))
    );
    program.validate().unwrap();
}

#[test]
fn reports_invalid_statement() {
    let error = compile("value ? 1").unwrap_err();
    assert!(error.message.contains("unexpected character"));
}

#[test]
fn compiles_extended_control_flow() {
    let source = r#"
value = 10 % 3
terminal = object.find("terminal")
value--
if false && missing {
    terminal.println("bad")
} else if true or missing {
    terminal.println(value)
}
while value < 10 {
    value++
    if value == 3 { continue }
    if value == 4 { break }
}
"#;
    let program = compile(source).unwrap();
    assert!(program.tokens.iter().any(|token| token == &Token::Modulo));
    assert!(
        program
            .tokens
            .iter()
            .filter(|token| matches!(token, Token::Jump(_)))
            .count()
            >= 4
    );
    program.validate().unwrap();
}

#[test]
fn rejects_loop_control_outside_loop() {
    assert!(
        compile("break\n")
            .unwrap_err()
            .message
            .contains("inside while")
    );
    assert!(
        compile("continue\n")
            .unwrap_err()
            .message
            .contains("inside while")
    );
}

#[test]
fn compiles_collections_indexing_and_length() {
    let source = r#"
items = [1, 2, 3]
terminal = object.find("terminal")
items[0] = 10
user = { name: "Ada", age: 18 }
user["age"] = 19
total = #items + #user
terminal.println(items[0] + user["age"])
"#;
    let program = compile(source).unwrap();
    assert!(program.tokens.contains(&Token::MakeArray(3)));
    assert!(program.tokens.contains(&Token::MakeMap(2)));
    assert!(program.tokens.contains(&Token::IndexGet));
    assert!(program.tokens.contains(&Token::IndexSet));
    assert!(program.tokens.contains(&Token::Length));
    program.validate().unwrap();
}

#[test]
fn compiles_functions_classes_and_uniform_object_creation() {
    let source = r#"
func twice(value) { return value * 2 }

class Counter {
    value = 0
    private secret = 4
    func increment(amount) {
        this.value = this.value + amount
        return this.value
    }
}

counter = object.create("Counter", { value: twice(5) })
alias link counter
result = alias.increment(2)
"#;
    let program = compile(source).unwrap();
    assert!(
        program
            .tokens
            .iter()
            .any(|token| matches!(token, Token::DefineFunction { name, .. } if name == "twice"))
    );
    assert!(
        program
            .tokens
            .iter()
            .any(|token| matches!(token, Token::DefineClass { name, .. } if name == "Counter"))
    );
    assert!(
        program
            .tokens
            .iter()
            .any(|token| matches!(token, Token::DefineMethod { name, .. } if name == "increment"))
    );
    assert!(program.tokens.contains(&Token::BindLink {
        name: "alias".to_owned(),
        target: "counter".to_owned(),
    }));
    assert!(compile("item = new Counter()").is_err());
    program.validate().unwrap();
}

#[test]
fn expands_import_once_and_include_each_time() {
    let source = "import \"math\"\nimport \"math\"\ninclude \"values\"\nresult = twice(a)\n";
    let program = compile_with_loader(source, |name| match name {
        "math" => Ok("func twice(value) { return value * 2 }\n".to_owned()),
        "values" => Ok("a = 3\n".to_owned()),
        _ => Err("not found".to_owned()),
    })
    .unwrap();
    assert_eq!(
        program
            .tokens
            .iter()
            .filter(|token| matches!(token, Token::DefineFunction { name, .. } if name == "twice"))
            .count(),
        1
    );
    assert!(compile(source).unwrap_err().message.contains("loader"));
}

#[test]
fn interactive_compilation_echoes_expressions_but_not_assignments() {
    let expression = compile_interactive("answer + 1").unwrap();
    assert_eq!(
        expression.tokens,
        vec![
            Token::LoadIdentity("terminal".to_owned()),
            Token::Load("answer".to_owned()),
            Token::Push(Value::Integer(1)),
            Token::Add,
            Token::ObjectCall {
                method: "println".to_owned(),
                arguments: 1,
            },
            Token::Pop,
            Token::Halt,
        ]
    );
    let assignment = compile_interactive("answer = 42").unwrap();
    assert_eq!(
        assignment.tokens,
        vec![
            Token::Push(Value::Integer(42)),
            Token::Store("answer".to_owned()),
            Token::Halt,
        ]
    );
}
