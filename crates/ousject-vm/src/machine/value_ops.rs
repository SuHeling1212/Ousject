fn finite_math_number(value: &Value) -> Result<f64, VmError> {
    let number = numeric_float(value)?;
    if !number.is_finite() {
        return Err(VmError::TypeError("math inputs must be finite numbers"));
    }
    Ok(number)
}

fn finite_math_result(value: f64) -> Result<Value, VmError> {
    if !value.is_finite() {
        return Err(VmError::TypeError(
            "math result is outside the finite range",
        ));
    }
    Ok(Value::Float(oms_types::FloatValue::new(value)))
}

fn rounded_math_value(value: &Value, round: fn(f64) -> f64) -> Result<Value, VmError> {
    match value {
        Value::Integer(_) => Ok(value.clone()),
        Value::Float(_) => finite_math_result(round(finite_math_number(value)?)),
        _ => Err(VmError::TypeError("math function requires a number")),
    }
}

fn numeric_order(left: &Value, right: &Value) -> Result<std::cmp::Ordering, VmError> {
    match (left, right) {
        (Value::Integer(left), Value::Integer(right)) => Ok(left.cmp(right)),
        (Value::Integer(integer), Value::Float(_)) => {
            Ok(compare_integer_float(*integer, finite_math_number(right)?))
        }
        (Value::Float(_), Value::Integer(integer)) => {
            Ok(compare_integer_float(*integer, finite_math_number(left)?).reverse())
        }
        (Value::Float(_), Value::Float(_)) => finite_math_number(left)?
            .partial_cmp(&finite_math_number(right)?)
            .ok_or(VmError::TypeError(
                "math comparison requires finite numbers",
            )),
        _ => Err(VmError::TypeError("math comparison requires numbers")),
    }
}

#[allow(clippy::cast_possible_truncation)]
fn compare_integer_float(integer: i64, float: f64) -> std::cmp::Ordering {
    if float >= 9_223_372_036_854_775_808.0 {
        return std::cmp::Ordering::Less;
    }
    if float < -9_223_372_036_854_775_808.0 {
        return std::cmp::Ordering::Greater;
    }
    let truncated = float.trunc() as i64;
    match integer.cmp(&truncated) {
        std::cmp::Ordering::Equal if float.fract() > 0.0 => std::cmp::Ordering::Less,
        std::cmp::Ordering::Equal if float.fract() < 0.0 => std::cmp::Ordering::Greater,
        ordering => ordering,
    }
}

fn value_length(value: &Value) -> Result<i64, VmError> {
    let length = match value {
        Value::Text(value) => value.chars().count(),
        Value::Bytes(value) => value.len(),
        Value::Array(value) => value.len(),
        Value::Map(value) | Value::Record(value) => value.len(),
        _ => return Err(VmError::TypeError("value has no length")),
    };
    i64::try_from(length).map_err(|_| VmError::TypeError("length exceeds integer range"))
}

fn compare(token: &Token, left: &Value, right: &Value) -> Result<bool, VmError> {
    match token {
        Token::Equal => Ok(left == right),
        Token::NotEqual => Ok(left != right),
        Token::Less | Token::LessEqual | Token::Greater | Token::GreaterEqual => {
            let left = numeric_float(left)?;
            let right = numeric_float(right)?;
            if left.is_nan() || right.is_nan() {
                return Err(VmError::TypeError("NaN cannot be ordered"));
            }
            Ok(match token {
                Token::Less => left < right,
                Token::LessEqual => left <= right,
                Token::Greater => left > right,
                Token::GreaterEqual => left >= right,
                _ => unreachable!(),
            })
        }
        _ => Err(VmError::TypeError("token is not a comparison")),
    }
}

#[allow(clippy::cast_precision_loss)]
fn numeric_float(value: &Value) -> Result<f64, VmError> {
    match value {
        Value::Integer(value) => Ok(*value as f64),
        Value::Float(value) => Ok(value.get()),
        _ => Err(VmError::TypeError(
            "numeric operation requires integers or floats",
        )),
    }
}
