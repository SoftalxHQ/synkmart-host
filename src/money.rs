use crate::error::{AppError, AppResult};
use serde::{Deserialize, Deserializer};
use serde_json::Value;

pub fn parse_money(raw: &str) -> AppResult<f64> {
    let cleaned = raw.trim().replace(',', "");
    if cleaned.is_empty() {
        return Ok(0.0);
    }
    cleaned
        .parse::<f64>()
        .map_err(|_| AppError::bad("Amount must be a number."))
}

pub fn format_money(value: f64) -> String {
    format!("{value:.2}")
}

/// Accept JSON string or number into a money string.
pub fn de_amount<'de, D: Deserializer<'de>>(deserializer: D) -> Result<String, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok("0".into()),
        Some(Value::String(s)) => Ok(s),
        Some(Value::Number(n)) => Ok(n.to_string()),
        Some(other) => Ok(other.to_string()),
    }
}

pub fn de_opt_amount<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    match Option::<Value>::deserialize(deserializer)? {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(Value::Number(n)) => Ok(Some(n.to_string())),
        Some(other) => Ok(Some(other.to_string())),
    }
}
