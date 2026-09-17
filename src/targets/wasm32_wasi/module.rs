//! Only the sections this compiler needs are written, and any that would
//! come out empty are left out entirely.

use super::{Code, END, FUNC_TYPE, I32, I32_CONST};
use crate::targets::bytes::{Bytes, len32};

/// Section ids, in the order a module must list them.
const TYPE: u8 = 1;
const IMPORT: u8 = 2;
const FUNCTION: u8 = 3;
const MEMORY: u8 = 5;
const EXPORT: u8 = 7;
const CODE: u8 = 10;
const DATA: u8 = 11;

const KIND_FUNC: u8 = 0x00;
const KIND_MEMORY: u8 = 0x02;

const WASI: &str = "wasi_snapshot_preview1";
const PAGE_SIZE: u32 = 65536;

pub(super) fn module(code: &Code) -> Vec<u8> {
    // Imports come first in both index spaces, so their count is also the
    // index of the one function this compiler writes, and of its type.
    let start = len32(code.imports.len());
    let mut out = Bytes::default();
    out.bytes(b"\0asm");
    out.bytes(&[1, 0, 0, 0]);

    types(&mut out, &code.imports);
    out.section(IMPORT, |s| {
        s.uleb(start);
        for (index, name) in code.imports.iter().enumerate() {
            s.name(WASI);
            s.name(name);
            s.byte(KIND_FUNC);
            s.uleb(len32(index));
        }
    });
    out.section(FUNCTION, |s| {
        s.uleb(1);
        s.uleb(start);
    });
    if code.memory {
        out.section(MEMORY, |s| {
            s.uleb(1);
            s.byte(0x00);
            s.uleb(pages(code));
        });
    }
    exports(&mut out, code.memory, start);
    body(&mut out, code);
    data(&mut out, code);

    out.finish()
}

/// One signature per imported function, then `_start`.
fn types(out: &mut Bytes, imports: &[&str]) {
    out.section(TYPE, |s| {
        s.uleb(len32(imports.len()) + 1);
        for name in imports {
            let (params, returns) = match *name {
                "fd_write" => (4, true),   // (fd, iovs, iovs_len, nwritten)
                "proc_exit" => (1, false), // (status)
                other => panic!("no signature for `{other}`"),
            };
            signature(s, params, returns);
        }
        signature(s, 0, false); // _start()
    });
}

/// WASI finds these by name. Memory is only exported when something reads
/// it, which for a program that prints nothing is never.
fn exports(out: &mut Bytes, memory: bool, start: u32) {
    out.section(EXPORT, |s| {
        s.uleb(1 + u32::from(memory));
        if memory {
            s.name("memory");
            s.byte(KIND_MEMORY);
            s.uleb(0);
        }
        s.name("_start");
        s.byte(KIND_FUNC);
        s.uleb(start);
    });
}

/// One function: its locals, the instructions, and the terminating `end`.
fn body(out: &mut Bytes, code: &Code) {
    out.section(CODE, |s| {
        s.uleb(1);
        s.sized(|f| {
            f.uleb(u32::from(code.locals > 0));
            if code.locals > 0 {
                f.uleb(code.locals);
                f.byte(I32);
            }
            f.bytes(&code.body);
            f.byte(END);
        });
    });
}

/// Each region of memory and the constant offset it loads at.
fn data(out: &mut Bytes, code: &Code) {
    out.section(DATA, |s| {
        if code.data.is_empty() {
            return;
        }
        s.uleb(len32(code.data.len()));
        for segment in &code.data {
            s.uleb(0); // Active, in memory 0.
            s.byte(I32_CONST);
            s.sleb(segment.offset.cast_signed());
            s.byte(END);
            s.uleb(len32(segment.bytes.len()));
            s.bytes(&segment.bytes);
        }
    });
}

fn signature(out: &mut Bytes, params: usize, returns_i32: bool) {
    out.byte(FUNC_TYPE);
    out.uleb(len32(params));
    out.bytes(&vec![I32; params]);
    out.uleb(u32::from(returns_i32));
    if returns_i32 {
        out.byte(I32);
    }
}

fn pages(code: &Code) -> u32 {
    let high = code
        .data
        .iter()
        .map(|segment| segment.offset + len32(segment.bytes.len()))
        .max()
        .unwrap_or(0);
    high.div_ceil(PAGE_SIZE).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::Target;

    fn hello() -> Vec<u8> {
        crate::compile(Target::Wasm32Wasi, &crate::hello_world())
            .pop()
            .unwrap()
            .bytes
    }

    #[test]
    fn it_starts_with_the_wasm_preamble() {
        assert_eq!(&hello()[..8], b"\0asm\x01\0\0\0");
    }

    #[test]
    fn sections_are_in_ascending_order() {
        let module = hello();
        let (mut ids, mut at) = (Vec::new(), 8);
        while at < module.len() {
            ids.push(module[at]);
            assert!(
                module[at + 1] < 0x80,
                "section {} is too long to skip",
                ids.len()
            );
            at += 2 + usize::from(module[at + 1]);
        }
        assert_eq!(ids, [TYPE, IMPORT, FUNCTION, MEMORY, EXPORT, CODE, DATA]);
    }

    #[test]
    fn one_page_holds_a_greeting() {
        assert_eq!(pages(&super::super::lower::lower(&crate::hello_world())), 1);
    }
}
