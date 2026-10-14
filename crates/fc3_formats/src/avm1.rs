//! ActionScript 2 bytecode the interface movies need changed to run in a
//! standard player.
//!
//! The movies call the engine with Flash Lite's `FSCommand2` (opcode 0x2D:
//! it pops an argument count, the command, then the arguments, and pushes a
//! result), which desktop players don't know. [`replace_fscommand2`] puts a
//! call of a global function in its place: the stack `FSCommand2` reads is
//! exactly what `CallFunction` reads after the function's name, so the
//! function gets the command, then its arguments, and its result stays on
//! the stack as `FSCommand2`'s would.
//!
//! The call is longer than the opcode, so the code after it moves: branch
//! offsets, function bodies' sizes and `with`/`try` block sizes that span a
//! replaced opcode are adjusted. (`WaitForFrame` skips count actions, not
//! bytes; no movie uses it, and one spanning a replacement is refused.)

use anyhow::{bail, ensure, Result};

/// `FSCommand2`.
pub const FSCOMMAND2: u8 = 0x2D;
const PUSH: u8 = 0x96;
const CALL_FUNCTION: u8 = 0x3D;
const JUMP: u8 = 0x99;
const IF: u8 = 0x9D;
const DEFINE_FUNCTION: u8 = 0x9B;
const DEFINE_FUNCTION2: u8 = 0x8E;
const WITH: u8 = 0x94;
const TRY: u8 = 0x8F;
const WAIT_FOR_FRAME: u8 = 0x8A;
const WAIT_FOR_FRAME2: u8 = 0x8D;

/// One action: where it starts, its opcode and its size (header and
/// payload; a function's body follows it as actions of its own).
struct Action {
    at: usize,
    op: u8,
    size: usize,
}

fn actions(code: &[u8]) -> Result<Vec<Action>> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < code.len() {
        let op = code[at];
        let size = if op >= 0x80 {
            ensure!(at + 3 <= code.len(), "action at {at} cut short");
            3 + u16::from_le_bytes([code[at + 1], code[at + 2]]) as usize
        } else {
            1
        };
        ensure!(at + size <= code.len(), "action at {at} runs past the code");
        out.push(Action { at, op, size });
        at += size;
    }
    Ok(out)
}

/// `code` with every `FSCommand2` turned into a call of the global
/// function `function` (unchanged when there is none).
pub fn replace_fscommand2(code: &[u8], function: &str) -> Result<Vec<u8>> {
    let list = actions(code)?;
    let sites: Vec<usize> = list.iter().filter(|a| a.op == FSCOMMAND2).map(|a| a.at).collect();
    if sites.is_empty() {
        return Ok(code.to_vec());
    }
    let mut call = vec![PUSH];
    call.extend_from_slice(&((function.len() + 2) as u16).to_le_bytes());
    call.push(0);
    call.extend_from_slice(function.as_bytes());
    call.push(0);
    call.push(CALL_FUNCTION);
    let grow = call.len() - 1;
    // Where an old offset lands: past every replacement before it.
    let moved = |x: usize| x + grow * sites.partition_point(|&s| s < x);
    let size_to = |from: usize, len: usize| -> Result<u16> {
        let n = moved(from + len) - moved(from);
        u16::try_from(n).map_err(|_| anyhow::anyhow!("a block grows past 64 KB"))
    };
    let mut out = Vec::with_capacity(code.len() + grow * sites.len());
    for (i, a) in list.iter().enumerate() {
        let bytes = &code[a.at..a.at + a.size];
        let end = a.at + a.size;
        match a.op {
            FSCOMMAND2 => out.extend_from_slice(&call),
            JUMP | IF if a.size == 5 => {
                let off = i16::from_le_bytes([bytes[3], bytes[4]]) as isize;
                let target = end as isize + off;
                ensure!(target >= 0 && target as usize <= code.len(), "branch at {} leaves the code", a.at);
                let new = moved(target as usize) as isize - moved(end) as isize;
                let new = i16::try_from(new).map_err(|_| anyhow::anyhow!("branch at {} too far once moved", a.at))?;
                out.extend_from_slice(&bytes[..3]);
                out.extend_from_slice(&new.to_le_bytes());
            }
            DEFINE_FUNCTION | DEFINE_FUNCTION2 | WITH if a.size >= 5 => {
                // (The body's size is the payload's last two bytes.)
                let n = u16::from_le_bytes([bytes[a.size - 2], bytes[a.size - 1]]) as usize;
                out.extend_from_slice(&bytes[..a.size - 2]);
                out.extend_from_slice(&size_to(end, n)?.to_le_bytes());
            }
            TRY if a.size >= 10 => {
                let u = |k: usize| u16::from_le_bytes([bytes[k], bytes[k + 1]]) as usize;
                let (t, c, f) = (u(4), u(6), u(8));
                out.extend_from_slice(&bytes[..4]);
                out.extend_from_slice(&size_to(end, t)?.to_le_bytes());
                out.extend_from_slice(&size_to(end + t, c)?.to_le_bytes());
                out.extend_from_slice(&size_to(end + t + c, f)?.to_le_bytes());
                out.extend_from_slice(&bytes[10..]);
            }
            WAIT_FOR_FRAME | WAIT_FOR_FRAME2 => {
                let skip = *bytes.last().unwrap_or(&0) as usize;
                if list[i + 1..].iter().take(skip).any(|b| b.op == FSCOMMAND2) {
                    bail!("WaitForFrame at {} skips an engine call", a.at);
                }
                out.extend_from_slice(bytes);
            }
            _ => out.extend_from_slice(bytes),
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn push_str(s: &str) -> Vec<u8> {
        let mut v = vec![PUSH];
        v.extend_from_slice(&((s.len() + 2) as u16).to_le_bytes());
        v.push(0);
        v.extend_from_slice(s.as_bytes());
        v.push(0);
        v
    }

    #[test]
    fn an_engine_call_becomes_a_function_call_and_branches_across_it_follow() {
        // if (...) skip; FSCommand2; Pop; end    (the If jumps over the call)
        let mut code = vec![IF, 2, 0, 2, 0]; // to after the FSCommand2 and Pop
        code.push(FSCOMMAND2);
        code.push(0x17); // Pop
        code.push(0); // End
        let out = replace_fscommand2(&code, "f").unwrap();
        let call = [push_str("f"), vec![CALL_FUNCTION]].concat();
        assert_eq!(&out[5..5 + call.len()], &call[..]);
        // The If now skips the whole call and the Pop.
        assert_eq!(i16::from_le_bytes([out[3], out[4]]) as usize, call.len() + 1);
        assert_eq!(out.len(), code.len() + call.len() - 1);
    }

    #[test]
    fn a_function_holding_an_engine_call_grows_and_a_backward_jump_follows() {
        // function () { FSCommand2 }  then a jump back to the start
        let mut code = vec![DEFINE_FUNCTION, 5, 0, 0, 0, 0, 1, 0]; // name "", no params, body 1 byte
        code.push(FSCOMMAND2);
        let back = -(code.len() as i16 + 5);
        code.extend_from_slice(&[JUMP, 2, 0]);
        code.extend_from_slice(&back.to_le_bytes());
        let out = replace_fscommand2(&code, "fire").unwrap();
        let grown = push_str("fire").len() + 1;
        assert_eq!(u16::from_le_bytes([out[6], out[7]]) as usize, grown);
        let j = 8 + grown;
        assert_eq!(out[j], JUMP);
        assert_eq!(i16::from_le_bytes([out[j + 3], out[j + 4]]) as isize, -((j + 5) as isize));
    }

    #[test]
    fn code_without_engine_calls_is_left_alone() {
        let code = vec![JUMP, 2, 0, 1, 0, 0x17, 0];
        assert_eq!(replace_fscommand2(&code, "f").unwrap(), code);
    }
}
