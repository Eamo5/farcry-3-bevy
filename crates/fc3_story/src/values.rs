//! Lenient conversions for native arguments, as the engine's marshallers
//! accept them: ids and names arrive as strings or numbers (or nil), flags
//! as booleans or 0/1.

use mlua::{FromLua, IntoLua, Lua, Value};

/// Any scalar as text (nil -> empty).
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct Text(pub String);

impl Text {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// An entity / block / layer id (decimal), if it is one.
    pub fn id(&self) -> Option<u64> {
        self.0.trim().parse().ok().filter(|&i| i != u64::MAX)
    }

    /// A sound id (`"0x0016DE53"` or decimal).
    pub fn sound(&self) -> Option<u32> {
        let s = self.0.trim();
        match s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
            Some(hex) => u32::from_str_radix(hex, 16).ok(),
            None => s.parse().ok(),
        }
        .filter(|&i| i != u32::MAX)
    }

    /// A group (`#XXXXXXXX` name hash).
    pub fn group(&self) -> Option<u32> {
        self.0.strip_prefix('#').and_then(|h| u32::from_str_radix(h, 16).ok())
    }
}

pub fn number_text(n: f64) -> String {
    if n.fract() == 0.0 && n.abs() < 1e15 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

impl FromLua for Text {
    fn from_lua(v: Value, _: &Lua) -> mlua::Result<Self> {
        Ok(Text(match v {
            Value::String(s) => s.to_string_lossy(),
            Value::Integer(i) => i.to_string(),
            Value::Number(n) => number_text(n),
            Value::Boolean(b) => b.to_string(),
            _ => String::new(),
        }))
    }
}

/// A truth value as the scripts pass them (booleans, 0/1, "true").
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Flag(pub bool);

impl FromLua for Flag {
    fn from_lua(v: Value, _: &Lua) -> mlua::Result<Self> {
        Ok(Flag(match v {
            Value::Nil => false,
            Value::Boolean(b) => b,
            Value::Integer(i) => i != 0,
            Value::Number(n) => n != 0.0,
            Value::String(s) => matches!(s.to_string_lossy().as_str(), "true" | "1"),
            _ => true,
        }))
    }
}

/// A number (nil or text that isn't one -> none).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Num(pub Option<f64>);

impl Num {
    pub fn or(self, d: f64) -> f64 {
        self.0.unwrap_or(d)
    }
}

impl FromLua for Num {
    fn from_lua(v: Value, _: &Lua) -> mlua::Result<Self> {
        Ok(Num(match v {
            Value::Integer(i) => Some(i as f64),
            Value::Number(n) => Some(n),
            Value::String(s) => s.to_string_lossy().trim().parse().ok(),
            Value::Boolean(b) => Some(b as u8 as f64),
            _ => None,
        }))
    }
}

/// A value handed to a script callback.
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Nil,
    Bool(bool),
    Num(f64),
    Str(String),
}

impl Arg {
    pub fn id(id: u64) -> Arg {
        Arg::Str(id.to_string())
    }
}

impl From<&str> for Arg {
    fn from(s: &str) -> Arg {
        Arg::Str(s.to_string())
    }
}

impl From<String> for Arg {
    fn from(s: String) -> Arg {
        Arg::Str(s)
    }
}

impl From<f64> for Arg {
    fn from(n: f64) -> Arg {
        Arg::Num(n)
    }
}

impl From<bool> for Arg {
    fn from(b: bool) -> Arg {
        Arg::Bool(b)
    }
}

impl IntoLua for Arg {
    fn into_lua(self, lua: &Lua) -> mlua::Result<Value> {
        Ok(match self {
            Arg::Nil => Value::Nil,
            Arg::Bool(b) => Value::Boolean(b),
            Arg::Num(n) => Value::Number(n),
            Arg::Str(s) => Value::String(lua.create_string(&s)?),
        })
    }
}

/// An oasis string reference as scripts pass them: `{section, item, id}`,
/// or a bare id.
pub fn oasis_id(v: &Value) -> Option<u32> {
    match v {
        Value::Table(t) => t.get::<Num>("id").ok().and_then(|n| n.0).filter(|&n| n >= 0.0).map(|n| n as u32),
        Value::Integer(i) => u32::try_from(*i).ok(),
        Value::Number(n) if *n >= 0.0 => Some(*n as u32),
        Value::String(s) => s.to_string_lossy().trim().parse().ok(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_and_sounds() {
        assert_eq!(Text("2074829410432770494".into()).id(), Some(2074829410432770494));
        assert_eq!(Text("18446744073709551615".into()).id(), None);
        assert_eq!(Text("0x0016DE53".into()).sound(), Some(0x0016DE53));
        assert_eq!(Text("#70B1A04C".into()).group(), Some(0x70B1A04C));
        assert_eq!(number_text(3.0), "3");
    }
}
