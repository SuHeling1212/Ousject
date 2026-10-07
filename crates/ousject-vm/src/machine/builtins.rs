fn arithmetic(token: &Token, left: Value, right: Value) -> Result<Value, VmError> {
    if matches!(left, Value::Float(_)) || matches!(right, Value::Float(_)) {
        let left = numeric_float(&left)?;
        let right = numeric_float(&right)?;
        if matches!(token, Token::Divide | Token::Modulo) && right == 0.0 {
            return Err(VmError::DivisionByZero);
        }
        let value = match token {
            Token::Add => left + right,
            Token::Subtract => left - right,
            Token::Multiply => left * right,
            Token::Divide => left / right,
            Token::Modulo => left % right,
            _ => return Err(VmError::TypeError("invalid arithmetic operands")),
        };
        return Ok(Value::Float(oms_types::FloatValue::new(value)));
    }
    match (token, left, right) {
        (Token::Add, Value::Integer(left), Value::Integer(right)) => left
            .checked_add(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Subtract, Value::Integer(left), Value::Integer(right)) => left
            .checked_sub(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Multiply, Value::Integer(left), Value::Integer(right)) => left
            .checked_mul(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Divide | Token::Modulo, Value::Integer(_), Value::Integer(0)) => {
            Err(VmError::DivisionByZero)
        }
        (Token::Divide, Value::Integer(left), Value::Integer(right)) => left
            .checked_div(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Modulo, Value::Integer(left), Value::Integer(right)) => left
            .checked_rem(right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Add, Value::Text(mut left), Value::Text(right)) => {
            left.push_str(&right);
            Ok(Value::Text(left))
        }
        _ => Err(VmError::TypeError("invalid arithmetic operands")),
    }
}

fn arithmetic_ref(token: &Token, left: &Value, right: &Value) -> Result<Value, VmError> {
    if matches!(left, Value::Float(_)) || matches!(right, Value::Float(_)) {
        let left = numeric_float(left)?;
        let right = numeric_float(right)?;
        if matches!(token, Token::Divide | Token::Modulo) && right == 0.0 {
            return Err(VmError::DivisionByZero);
        }
        let value = match token {
            Token::Add => left + right,
            Token::Subtract => left - right,
            Token::Multiply => left * right,
            Token::Divide => left / right,
            Token::Modulo => left % right,
            _ => return Err(VmError::TypeError("invalid arithmetic operands")),
        };
        return Ok(Value::Float(oms_types::FloatValue::new(value)));
    }
    match (token, left, right) {
        (Token::Add, Value::Integer(left), Value::Integer(right)) => left
            .checked_add(*right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Subtract, Value::Integer(left), Value::Integer(right)) => left
            .checked_sub(*right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Multiply, Value::Integer(left), Value::Integer(right)) => left
            .checked_mul(*right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Divide | Token::Modulo, Value::Integer(_), Value::Integer(0)) => {
            Err(VmError::DivisionByZero)
        }
        (Token::Divide, Value::Integer(left), Value::Integer(right)) => left
            .checked_div(*right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Modulo, Value::Integer(left), Value::Integer(right)) => left
            .checked_rem(*right)
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow")),
        (Token::Add, Value::Text(left), Value::Text(right)) => {
            let mut result = String::with_capacity(left.len().saturating_add(right.len()));
            result.push_str(left);
            result.push_str(right);
            Ok(Value::Text(result))
        }
        _ => Err(VmError::TypeError("invalid arithmetic operands")),
    }
}

fn execute_collection_token(token: &Token, stack: &mut Vec<Value>) -> Result<(), VmError> {
    match token {
        Token::MakeArray(count) => {
            let count =
                usize::try_from(*count).map_err(|_| VmError::TypeError("array is too large"))?;
            let start = stack
                .len()
                .checked_sub(count)
                .ok_or(VmError::StackUnderflow)?;
            let values = stack.split_off(start);
            stack.push(Value::Array(values));
        }
        Token::MakeMap(count) => {
            let count =
                usize::try_from(*count).map_err(|_| VmError::TypeError("map is too large"))?;
            let item_count = count
                .checked_mul(2)
                .ok_or(VmError::TypeError("map is too large"))?;
            if stack.len() < item_count {
                return Err(VmError::StackUnderflow);
            }
            let start = stack.len() - item_count;
            if (0..count).any(|index| !matches!(&stack[start + index * 2], Value::Text(_))) {
                return Err(VmError::TypeError("map key must be text"));
            }
            let items = stack.split_off(start);
            let mut values = BTreeMap::new();
            // Match the previous stack-pop order for duplicate keys: the first
            // pair in source order wins.
            for pair in items.chunks_exact(2).rev() {
                let Value::Text(key) = &pair[0] else {
                    unreachable!("map keys are validated before consuming the stack")
                };
                values.insert(key.clone(), pair[1].clone());
            }
            stack.push(Value::Map(values));
        }
        Token::IndexGet => {
            let start = stack.len().checked_sub(2).ok_or(VmError::StackUnderflow)?;
            let result = index_get(&stack[start], &stack[start + 1])?;
            stack.truncate(start);
            stack.push(result);
        }
        Token::IndexSet => {
            let start = stack.len().checked_sub(3).ok_or(VmError::StackUnderflow)?;
            validate_index_set(&stack[start], &stack[start + 1])?;
            let mut operands = stack.split_off(start);
            let value = operands.pop().expect("validated indexed assignment value");
            let index = operands.pop().expect("validated indexed assignment index");
            let collection = operands
                .pop()
                .expect("validated indexed assignment collection");
            stack.push(index_set(collection, index, value)?);
        }
        Token::IndexIncrement | Token::IndexDecrement => {
            let start = stack.len().checked_sub(2).ok_or(VmError::StackUnderflow)?;
            let current = index_get(&stack[start], &stack[start + 1])?;
            let operator = if matches!(token, Token::IndexIncrement) {
                Token::Add
            } else {
                Token::Subtract
            };
            let value = arithmetic_ref(&operator, &current, &Value::Integer(1))?;
            validate_index_set(&stack[start], &stack[start + 1])?;
            let mut operands = stack.split_off(start);
            let index = operands.pop().expect("validated indexed increment index");
            let collection = operands
                .pop()
                .expect("validated indexed increment collection");
            stack.push(index_set(collection, index, value)?);
        }
        Token::Length => {
            let index = stack.len().checked_sub(1).ok_or(VmError::StackUnderflow)?;
            let length = value_length(&stack[index])?;
            stack.pop();
            stack.push(Value::Integer(length));
        }
        _ => return Err(VmError::TypeError("token is not a collection operation")),
    }
    Ok(())
}

fn index_get(collection: &Value, index: &Value) -> Result<Value, VmError> {
    match (collection, index) {
        (Value::Array(values), Value::Integer(index)) => values
            .get(index_position(*index)?)
            .cloned()
            .ok_or(VmError::IndexOutOfBounds),
        (Value::Map(values) | Value::Record(values), Value::Text(key)) => values
            .get(key)
            .cloned()
            .ok_or_else(|| VmError::MissingKey(key.clone())),
        (Value::Text(value), Value::Integer(index)) => value
            .chars()
            .nth(index_position(*index)?)
            .map(|character| Value::Text(character.to_string()))
            .ok_or(VmError::IndexOutOfBounds),
        _ => Err(VmError::TypeError("value does not support this index")),
    }
}

fn validate_index_set(collection: &Value, index: &Value) -> Result<(), VmError> {
    match (collection, index) {
        (Value::Array(values), Value::Integer(index)) => values
            .get(index_position(*index)?)
            .map(|_| ())
            .ok_or(VmError::IndexOutOfBounds),
        (Value::Map(_) | Value::Record(_), Value::Text(_)) => Ok(()),
        _ => Err(VmError::TypeError(
            "value does not support indexed assignment",
        )),
    }
}

fn index_set(mut collection: Value, index: Value, value: Value) -> Result<Value, VmError> {
    match (&mut collection, index) {
        (Value::Array(values), Value::Integer(index)) => {
            let target = values
                .get_mut(index_position(index)?)
                .ok_or(VmError::IndexOutOfBounds)?;
            *target = value;
        }
        (Value::Map(values) | Value::Record(values), Value::Text(key)) => {
            values.insert(key, value);
        }
        _ => {
            return Err(VmError::TypeError(
                "value does not support indexed assignment",
            ));
        }
    }
    Ok(collection)
}

fn index_position(index: i64) -> Result<usize, VmError> {
    usize::try_from(index).map_err(|_| VmError::IndexOutOfBounds)
}

#[allow(clippy::too_many_lines)]
fn math_capability(name: &str, arguments: &[Value]) -> Result<Value, VmError> {
    match (name, arguments) {
        ("random", []) => random_float(),
        ("random_integer", [Value::Integer(minimum), Value::Integer(maximum)]) => {
            if minimum > maximum {
                return Err(VmError::TypeError(
                    "math.random_integer minimum exceeds maximum",
                ));
            }
            Ok(Value::Integer(random_integer_inclusive(
                *minimum, *maximum,
            )?))
        }
        ("abs", [Value::Integer(value)]) => value
            .checked_abs()
            .map(Value::Integer)
            .ok_or(VmError::TypeError("integer overflow in math.abs")),
        ("abs", [Value::Float(value)]) => finite_math_result(value.get().abs()),
        ("min", [left, right]) => Ok(if numeric_order(left, right)?.is_gt() {
            right.clone()
        } else {
            left.clone()
        }),
        ("max", [left, right]) => Ok(if numeric_order(left, right)?.is_lt() {
            right.clone()
        } else {
            left.clone()
        }),
        ("clamp", [value, minimum, maximum]) => {
            if numeric_order(minimum, maximum)?.is_gt() {
                return Err(VmError::TypeError("math.clamp minimum exceeds maximum"));
            }
            if numeric_order(value, minimum)?.is_lt() {
                Ok(minimum.clone())
            } else if numeric_order(value, maximum)?.is_gt() {
                Ok(maximum.clone())
            } else {
                Ok(value.clone())
            }
        }
        ("sqrt", [value]) => {
            let value = finite_math_number(value)?;
            if value < 0.0 {
                return Err(VmError::TypeError(
                    "math.sqrt requires a non-negative number",
                ));
            }
            finite_math_result(value.sqrt())
        }
        ("pow", [base, exponent]) => {
            let base = finite_math_number(base)?;
            let exponent = finite_math_number(exponent)?;
            finite_math_result(base.powf(exponent))
        }
        ("floor", [value]) => rounded_math_value(value, f64::floor),
        ("ceil", [value]) => rounded_math_value(value, f64::ceil),
        ("round", [value]) => rounded_math_value(value, f64::round),
        ("trunc", [value]) => rounded_math_value(value, f64::trunc),
        ("sin", [value]) => finite_math_result(finite_math_number(value)?.sin()),
        ("cos", [value]) => finite_math_result(finite_math_number(value)?.cos()),
        ("tan", [value]) => finite_math_result(finite_math_number(value)?.tan()),
        ("atan2", [y, x]) => {
            finite_math_result(finite_math_number(y)?.atan2(finite_math_number(x)?))
        }
        ("hypot", [x, y]) => {
            finite_math_result(finite_math_number(x)?.hypot(finite_math_number(y)?))
        }
        ("log", [value]) => {
            let value = finite_math_number(value)?;
            if value <= 0.0 {
                return Err(VmError::TypeError("math.log requires a positive number"));
            }
            finite_math_result(value.ln())
        }
        ("log2", [value]) => {
            let value = finite_math_number(value)?;
            if value <= 0.0 {
                return Err(VmError::TypeError("math.log2 requires a positive number"));
            }
            finite_math_result(value.log2())
        }
        ("log10", [value]) => {
            let value = finite_math_number(value)?;
            if value <= 0.0 {
                return Err(VmError::TypeError("math.log10 requires a positive number"));
            }
            finite_math_result(value.log10())
        }
        ("exp", [value]) => finite_math_result(finite_math_number(value)?.exp()),
        _ => Err(VmError::TypeError("invalid arguments to math capability")),
    }
}

#[allow(clippy::cast_precision_loss)]
fn random_float() -> Result<Value, VmError> {
    // 53 random bits are represented exactly by an IEEE-754 f64 significand.
    const DENOMINATOR: f64 = 9_007_199_254_740_992.0;
    let mantissa = random_u64()? >> 11;
    Ok(Value::Float(oms_types::FloatValue::new(
        mantissa as f64 / DENOMINATOR,
    )))
}

fn random_integer_inclusive(minimum: i64, maximum: i64) -> Result<i64, VmError> {
    const SIGN_BIT: u64 = 1_u64 << 63;
    let minimum_ordered = u64::from_ne_bytes(minimum.to_ne_bytes()) ^ SIGN_BIT;
    let maximum_ordered = u64::from_ne_bytes(maximum.to_ne_bytes()) ^ SIGN_BIT;
    let range = maximum_ordered
        .wrapping_sub(minimum_ordered)
        .wrapping_add(1);
    let offset = if range == 0 {
        random_u64()?
    } else {
        let rejection_threshold = range.wrapping_neg() % range;
        loop {
            let sample = random_u64()?;
            if sample >= rejection_threshold {
                break sample % range;
            }
        }
    };
    let ordered = minimum_ordered.wrapping_add(offset) ^ SIGN_BIT;
    Ok(i64::from_ne_bytes(ordered.to_ne_bytes()))
}

fn random_u64() -> Result<u64, VmError> {
    let mut bytes = [0_u8; 8];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut source| source.read_exact(&mut bytes))
        .map_err(|error| VmError::Provider(format!("system random source failed: {error}")))?;
    Ok(u64::from_ne_bytes(bytes))
}

fn text_capability(name: &str, arguments: &[Value], receiver: &Value) -> Result<Value, VmError> {
    let Value::Text(text) = receiver else {
        return Err(VmError::TypeError("text capability requires a Text Object"));
    };
    match (name, arguments) {
        ("slice", [Value::Integer(start), Value::Integer(length)]) => {
            let characters: Vec<char> = text.chars().collect();
            let start = usize::try_from(*start)
                .map_err(|_| VmError::TypeError("text slice start must be non-negative"))?;
            let length = usize::try_from(*length)
                .map_err(|_| VmError::TypeError("text slice length must be non-negative"))?;
            let end = start
                .checked_add(length)
                .filter(|end| *end <= characters.len())
                .ok_or(VmError::IndexOutOfBounds)?;
            Ok(Value::Text(characters[start..end].iter().collect()))
        }
        ("find", [Value::Text(needle)]) => {
            let position = text.find(needle).map(|byte| text[..byte].chars().count());
            Ok(position.map_or(Value::Integer(-1), |position| {
                Value::Integer(i64::try_from(position).unwrap_or(i64::MAX))
            }))
        }
        ("contains", [Value::Text(needle)]) => Ok(Value::Bool(text.contains(needle))),
        ("split", [Value::Text(delimiter)]) => {
            let parts = if delimiter.is_empty() {
                text.chars()
                    .map(|character| Value::Text(character.to_string()))
                    .collect()
            } else {
                text.split(delimiter)
                    .map(|part| Value::Text(part.to_owned()))
                    .collect()
            };
            Ok(Value::Array(parts))
        }
        ("replace_all", [Value::Text(from), Value::Text(to)]) => {
            Ok(Value::Text(text.replace(from, to)))
        }
        ("trim", []) => Ok(Value::Text(text.trim().to_owned())),
        ("lower", []) => Ok(Value::Text(text.to_lowercase())),
        ("upper", []) => Ok(Value::Text(text.to_uppercase())),
        ("utf8_bytes", []) => Ok(Value::Bytes(text.as_bytes().to_vec())),
        _ => Err(VmError::TypeError("invalid arguments to text capability")),
    }
}
