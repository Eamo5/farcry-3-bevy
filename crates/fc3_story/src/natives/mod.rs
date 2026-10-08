//! The engine functions the graphs call.
//!
//! Plain globals are registered under their names; singleton methods under
//! `Class:Method` and API-table functions under `Table.Function`, which
//! `glue.lua` turns into the objects scripts reach them through. Every native
//! works on the shared [`Core`]; none calls back into Lua (callbacks are
//! queued and run by `Story::pump`).

mod flow;
mod missions;
pub mod world;

use std::cell::RefCell;
use std::rc::Rc;

use fc3_domino::Domino;
use mlua::{FromLuaMulti, IntoLuaMulti, Lua, Table, Value};

use crate::core::{Callback, Core};

pub(crate) type Shared = Rc<RefCell<Core>>;

/// Registers natives on one Lua state.
pub(crate) struct Reg<'a> {
    pub d: &'a Domino,
    pub core: Shared,
}

impl Reg<'_> {
    /// A native working on the core.
    pub fn f<A, R, F>(&self, name: &str, f: F) -> mlua::Result<()>
    where
        A: FromLuaMulti,
        R: IntoLuaMulti,
        F: Fn(&mut Core, A) -> R + 'static,
    {
        let core = self.core.clone();
        self.d.register(name, move |_, a: A| Ok(f(&mut core.borrow_mut(), a)))
    }

    /// A native that also needs the Lua state (to build tables).
    pub fn lua<A, R, F>(&self, name: &str, f: F) -> mlua::Result<()>
    where
        A: FromLuaMulti,
        R: IntoLuaMulti,
        F: Fn(&Lua, &mut Core, A) -> mlua::Result<R> + 'static,
    {
        let core = self.core.clone();
        self.d.register(name, move |lua, a: A| f(lua, &mut core.borrow_mut(), a))
    }

    /// A native that only reports what a script asked (for the host to
    /// act on), returning nothing.
    pub fn command(&self, name: &str) -> mlua::Result<()> {
        let n = name.to_string();
        self.f(name, move |c, args: mlua::Variadic<crate::values::Text>| {
            c.emit(crate::Command::Native { name: n.clone(), args: args.into_iter().map(|t| t.0).collect() });
        })
    }

    /// A native returning a constant.
    pub fn constant<R: IntoLuaMulti + Clone + 'static>(&self, name: &str, value: R) -> mlua::Result<()> {
        self.d.register(name, move |_, _: mlua::MultiValue| Ok(value.clone()))
    }
}

/// A callback argument pair (box, method name).
pub(crate) fn callback(target: Value, method: Option<String>) -> Option<Callback> {
    match (target, method) {
        (Value::Table(target), Some(method)) if !method.is_empty() => Some(Callback { target, method }),
        _ => None,
    }
}

/// A Lua array of id strings.
pub(crate) fn id_list(lua: &Lua, ids: impl IntoIterator<Item = u64>) -> mlua::Result<Table> {
    lua.create_sequence_from(ids.into_iter().map(|i| i.to_string()))
}

/// A `{x, y, z}` vector.
pub(crate) fn vec3(lua: &Lua, v: [f32; 3]) -> mlua::Result<Table> {
    lua.create_sequence_from(v.iter().map(|&x| x as f64))
}

pub(crate) fn register(d: &Domino, core: &Shared) -> mlua::Result<()> {
    let r = Reg { d, core: core.clone() };
    flow::register(&r)?;
    missions::register(&r)?;
    world::register(&r)?;
    Ok(())
}
