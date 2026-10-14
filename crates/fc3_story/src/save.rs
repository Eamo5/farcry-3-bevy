//! The story's progress in a save: what the engine keeps (mission and block
//! states and acknowledgement, layer and block states, safehouses and radio
//! towers, unlocked groups, entities removed or killed, the chapter, where
//! the player respawns) and the scripts' `PersistentGlobals`.
//!
//! Loading rebuilds the scripts from scratch and replays the master graph,
//! as the original does: registered missions take their saved states, so
//! completed ones pass straight through and running ones start again from
//! their last checkpoint.

use mlua::{Lua, Table, Value};

/// A saved story.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct StorySave {
    /// Mission name (or block id) -> state name and whether acknowledged.
    pub missions: Vec<(String, String, bool)>,
    pub layers: Vec<(u64, u8)>,
    pub blocks: Vec<(u64, u8)>,
    pub safehouses: Vec<u64>,
    pub radio_towers: Vec<u64>,
    pub tags: Vec<String>,
    pub removed: Vec<u64>,
    pub dead: Vec<u64>,
    pub chapter: i32,
    /// The spawn point the player comes back at.
    pub respawn: Option<u64>,
    /// `PersistentGlobals`, as `path\ttype\tvalue` lines (see [`dump_table`]).
    pub persistent: Vec<String>,
}

fn esc(s: &str) -> String {
    s.replace('\\', "\\\\").replace('\n', "\\n").replace('\t', "\\t")
}

fn unesc(s: &str) -> String {
    let mut out = String::new();
    let mut it = s.chars();
    while let Some(c) = it.next() {
        if c == '\\' {
            match it.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some(o) => out.push(o),
                None => {}
            }
        } else {
            out.push(c);
        }
    }
    out
}

impl StorySave {
    pub fn to_text(&self) -> String {
        let mut out = String::from("fc3story 1\n");
        for (k, s, ack) in &self.missions {
            out += &format!("mission\t{}\t{s}\t{}\n", esc(k), *ack as u8);
        }
        for (l, s) in &self.layers {
            out += &format!("layer\t{l}\t{s}\n");
        }
        for (b, s) in &self.blocks {
            out += &format!("block\t{b}\t{s}\n");
        }
        for id in &self.safehouses {
            out += &format!("safehouse\t{id}\n");
        }
        for id in &self.radio_towers {
            out += &format!("tower\t{id}\n");
        }
        for t in &self.tags {
            out += &format!("tag\t{}\n", esc(t));
        }
        // (Not what the story spawned this session: its ids are made
        // again after a load, for others.)
        for id in self.removed.iter().filter(|&&id| !crate::spawned_id(id)) {
            out += &format!("removed\t{id}\n");
        }
        for id in self.dead.iter().filter(|&&id| !crate::spawned_id(id)) {
            out += &format!("dead\t{id}\n");
        }
        out += &format!("chapter\t{}\n", self.chapter);
        if let Some(r) = self.respawn {
            out += &format!("respawn\t{r}\n");
        }
        for line in &self.persistent {
            out += &format!("global\t{line}\n");
        }
        out
    }

    pub fn from_text(text: &str) -> Option<StorySave> {
        let mut lines = text.lines();
        if lines.next()? != "fc3story 1" {
            return None;
        }
        let mut s = StorySave::default();
        for line in lines {
            let (kind, rest) = line.split_once('\t').unwrap_or((line, ""));
            let f: Vec<&str> = rest.split('\t').collect();
            let id = |i: usize| f.get(i).and_then(|v| v.parse::<u64>().ok());
            match kind {
                "mission" if f.len() >= 3 => s.missions.push((unesc(f[0]), f[1].to_string(), f[2] == "1")),
                "layer" => s.layers.extend(id(0).zip(f.get(1).and_then(|v| v.parse().ok()))),
                "block" => s.blocks.extend(id(0).zip(f.get(1).and_then(|v| v.parse().ok()))),
                "safehouse" => s.safehouses.extend(id(0)),
                "tower" => s.radio_towers.extend(id(0)),
                "tag" => s.tags.push(unesc(rest)),
                "removed" => s.removed.extend(id(0).filter(|&i| !crate::spawned_id(i))),
                "dead" => s.dead.extend(id(0).filter(|&i| !crate::spawned_id(i))),
                "chapter" => s.chapter = rest.parse().unwrap_or(0),
                "respawn" => s.respawn = id(0),
                "global" => s.persistent.push(rest.to_string()),
                _ => {}
            }
        }
        Some(s)
    }
}

/// Plain data of a Lua table as lines: `path\ttype\tvalue`, the path's keys
/// separated by `\u{1f}` (numbers as `#n`), types `b`, `n`, `s`, `t` (an
/// empty table).
pub fn dump_table(t: &Table) -> Vec<String> {
    let mut out = Vec::new();
    dump_into(t, "", &mut out, 0);
    out.sort();
    out
}

fn dump_into(t: &Table, prefix: &str, out: &mut Vec<String>, depth: usize) {
    if depth > 16 {
        return;
    }
    let mut any = false;
    for (k, v) in t.pairs::<Value, Value>().flatten() {
        let key = match &k {
            Value::String(s) => esc(&s.to_string_lossy()).replace('\u{1f}', ""),
            Value::Integer(i) => format!("#{i}"),
            Value::Number(n) => format!("#{n}"),
            _ => continue,
        };
        let path = if prefix.is_empty() { key } else { format!("{prefix}\u{1f}{key}") };
        match v {
            Value::Boolean(b) => out.push(format!("{path}\tb\t{}", b as u8)),
            Value::Integer(i) => out.push(format!("{path}\tn\t{i}")),
            Value::Number(n) => out.push(format!("{path}\tn\t{n}")),
            Value::String(s) => out.push(format!("{path}\ts\t{}", esc(&s.to_string_lossy()))),
            Value::Table(sub) => dump_into(&sub, &path, out, depth + 1),
            _ => continue,
        }
        any = true;
    }
    if !any && !prefix.is_empty() {
        out.push(format!("{prefix}\tt\t"));
    }
}

/// Fills a table from [`dump_table`] lines.
pub fn load_table(lua: &Lua, root: &Table, lines: &[String]) -> mlua::Result<()> {
    for line in lines {
        let mut f = line.splitn(3, '\t');
        let (Some(path), Some(kind), value) = (f.next(), f.next(), f.next().unwrap_or("")) else { continue };
        let keys: Vec<&str> = path.split('\u{1f}').collect();
        let key_value = |k: &str| -> mlua::Result<Value> {
            match k.strip_prefix('#').and_then(|n| n.parse::<f64>().ok()) {
                Some(n) => Ok(Value::Number(n)),
                None => Ok(Value::String(lua.create_string(unesc(k))?)),
            }
        };
        let mut t = root.clone();
        for k in &keys[..keys.len().saturating_sub(1)] {
            let kv = key_value(k)?;
            t = match t.get::<Value>(kv.clone())? {
                Value::Table(sub) => sub,
                _ => {
                    let sub = lua.create_table()?;
                    t.set(kv, sub.clone())?;
                    sub
                }
            };
        }
        let Some(last) = keys.last() else { continue };
        let kv = key_value(last)?;
        let v = match kind {
            "b" => Value::Boolean(value == "1"),
            "n" => Value::Number(value.parse().unwrap_or(0.0)),
            "s" => Value::String(lua.create_string(unesc(value))?),
            "t" => Value::Table(lua.create_table()?),
            _ => continue,
        };
        t.set(kv, v)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_round_trip() {
        let lua = Lua::new();
        let t: Table = lua.load(r#"return { DOC = { done = true, count = 3, name = "a\tb", list = { "x", "y" }, empty = {} } }"#).eval().unwrap();
        let lines = dump_table(&t);
        let back = lua.create_table().unwrap();
        load_table(&lua, &back, &lines).unwrap();
        assert_eq!(dump_table(&back), lines);
        let doc: Table = back.get("DOC").unwrap();
        assert!(doc.get::<bool>("done").unwrap());
        assert_eq!(doc.get::<String>("name").unwrap(), "a\tb");
    }

    #[test]
    fn saves_round_trip() {
        let s = StorySave {
            missions: vec![("JAS_020_MAIN".into(), "Running".into(), true)],
            layers: vec![(0x207d, 2)],
            chapter: 1,
            respawn: Some(42),
            persistent: vec!["DOC\u{1f}done\tb\t1".into()],
            ..Default::default()
        };
        assert_eq!(StorySave::from_text(&s.to_text()).unwrap(), s);
    }

    #[test]
    fn saves_keep_no_spawned_ids() {
        let spawned = crate::SPAWNED_IDS | 0x2_0000_0000_0218;
        let s = StorySave { removed: vec![7, spawned], dead: vec![spawned, 9], ..Default::default() };
        let back = StorySave::from_text(&s.to_text()).unwrap();
        assert_eq!((back.removed, back.dead), (vec![7], vec![9]));
        // (Older saves that kept them.)
        let old = StorySave::from_text(&format!("fc3story 1\nremoved\t{spawned}\ndead\t{spawned}\nremoved\t5\n")).unwrap();
        assert_eq!((old.removed, old.dead), (vec![5], vec![]));
    }
}
