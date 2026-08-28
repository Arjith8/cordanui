/// Database value — mirrors `rusqlite::types::Value` but adds `From<&str>`
/// and other conveniences so existing call sites using `Value::from("…")`
/// keep working after the rusqlite migration.

#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<String> for Value {
    fn from(s: String) -> Self {
        Value::Text(s)
    }
}
impl From<&str> for Value {
    fn from(s: &str) -> Self {
        Value::Text(s.to_string())
    }
}
impl From<&String> for Value {
    fn from(s: &String) -> Self {
        Value::Text(s.clone())
    }
}
impl From<i64> for Value {
    fn from(i: i64) -> Self {
        Value::Integer(i)
    }
}
impl From<i32> for Value {
    fn from(i: i32) -> Self {
        Value::Integer(i64::from(i))
    }
}
impl From<f64> for Value {
    fn from(f: f64) -> Self {
        Value::Real(f)
    }
}
impl From<Vec<u8>> for Value {
    fn from(b: Vec<u8>) -> Self {
        Value::Blob(b)
    }
}
impl From<Option<String>> for Value {
    fn from(o: Option<String>) -> Self {
        match o {
            Some(s) => Value::Text(s),
            None => Value::Null,
        }
    }
}

impl From<rusqlite::types::Value> for Value {
    fn from(v: rusqlite::types::Value) -> Self {
        match v {
            rusqlite::types::Value::Null => Value::Null,
            rusqlite::types::Value::Integer(i) => Value::Integer(i),
            rusqlite::types::Value::Real(f) => Value::Real(f),
            rusqlite::types::Value::Text(s) => Value::Text(s),
            rusqlite::types::Value::Blob(b) => Value::Blob(b),
        }
    }
}
impl From<Value> for rusqlite::types::Value {
    fn from(v: Value) -> Self {
        match v {
            Value::Null => rusqlite::types::Value::Null,
            Value::Integer(i) => rusqlite::types::Value::Integer(i),
            Value::Real(f) => rusqlite::types::Value::Real(f),
            Value::Text(s) => rusqlite::types::Value::Text(s),
            Value::Blob(b) => rusqlite::types::Value::Blob(b),
        }
    }
}

impl rusqlite::types::ToSql for Value {
    fn to_sql(&self) -> rusqlite::Result<rusqlite::types::ToSqlOutput<'_>> {
        let rv: rusqlite::types::Value = self.clone().into();
        Ok(rusqlite::types::ToSqlOutput::Owned(rv))
    }
}

