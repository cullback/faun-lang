//! How a known value is encoded.
//!
//! The pool is one byte buffer and a value is a range into it. Everything is
//! self-delimiting, so no length is stored anywhere except where a value's
//! own shape needs one:
//!
//! - **spine-shaped** — a count, then that many element encodings
//! - **one constructor** — its fields, with no tag to choose between them
//! - **otherwise** — a tag, then its fields
//!
//! A type is spine-shaped when it has two constructors, one taking nothing
//! and one taking exactly one field of the type itself. That is `Nat` and it
//! is `List`, which is the point: `Nat` is the member whose elements carry
//! nothing, so all that survives is the count, and a chain of `Succ` costs
//! the logarithm of its value rather than its value.
//!
//! The encoding is structural — which constructor, which fields. It is not a
//! layout. A target is free to hold the same value as a word, as a view into
//! static memory, or as a counted cell, and [`crate::ir`] is where that is
//! decided.

use super::{CtorId, Program, TypeId};

/// How a type's values are encoded, read off its declaration.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Shape {
    /// `nil` takes nothing; `cons` takes the rest of the value at `recursive`
    /// and carries its element in whatever fields remain.
    Spine {
        nil: CtorId,
        cons: CtorId,
        recursive: usize,
    },
    Tagged,
}

/// A value under construction, before it is encoded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Value(pub CtorId, pub Vec<Self>);

#[must_use]
pub fn shape(program: &Program, id: TypeId) -> Shape {
    let ctors = program.type_(id).ctors;
    if ctors.len() != 2 {
        return Shape::Tagged;
    }
    let (first, second) = (program.ctor_at(id, 0), program.ctor_at(id, 1));
    for (nil, cons) in [(first, second), (second, first)] {
        if !program.fields(nil).is_empty() {
            continue;
        }
        let mut recursive = program
            .fields(cons)
            .iter()
            .enumerate()
            .filter(|(_, field)| **field == id);
        if let (Some((at, _)), None) = (recursive.next(), recursive.next()) {
            return Shape::Spine {
                nil,
                cons,
                recursive: at,
            };
        }
    }
    Shape::Tagged
}

/// Append `value` to `out`, answering how many elements it has when its type
/// is spine-shaped and zero otherwise.
///
/// # Panics
///
/// If `value` is not of type `id`.
pub(super) fn encode(program: &Program, id: TypeId, value: &Value, out: &mut Vec<u8>) -> u32 {
    match shape(program, id) {
        Shape::Spine {
            nil,
            cons,
            recursive,
        } => {
            // Count first, since a nested spine has no range end to stop at.
            // Walking twice beats holding the chain: it is already in hand.
            let mut count = 0u64;
            let mut rest = value;
            while rest.0 == cons {
                count += 1;
                rest = &rest.1[recursive];
            }
            assert_eq!(rest.0, nil, "a spine ends in its empty constructor");
            leb128(count, out);

            let mut rest = value;
            while rest.0 == cons {
                for (at, field) in rest.1.iter().enumerate() {
                    if at != recursive {
                        encode(program, program.fields(cons)[at], field, out);
                    }
                }
                rest = &rest.1[recursive];
            }
            u32::try_from(count).expect("a spine within 4G")
        }
        Shape::Tagged => {
            if program.type_(id).ctors.len() > 1 {
                leb128(u64::from(program.tag(value.0)), out);
            }
            for (field, at) in value.1.iter().zip(program.fields(value.0)) {
                encode(program, *at, field, out);
            }
            0
        }
    }
}

/// Read one value back, answering it and how many bytes it took.
///
/// This builds the value in full, so a chain as long as its own magnitude
/// costs that much: a number is better read with [`as_number`] and a run of
/// bytes with [`as_bytes`].
///
/// # Panics
///
/// If the bytes are not an encoding of this type, which the pool's own
/// writer cannot produce.
#[must_use]
pub fn decode(program: &Program, id: TypeId, bytes: &[u8]) -> (Value, usize) {
    match shape(program, id) {
        Shape::Spine {
            nil,
            cons,
            recursive,
        } => decode_spine(program, bytes, nil, cons, recursive),
        Shape::Tagged => decode_tagged(program, id, bytes),
    }
}

fn decode_spine(
    program: &Program,
    bytes: &[u8],
    nil: CtorId,
    cons: CtorId,
    recursive: usize,
) -> (Value, usize) {
    {
        {
            let (count, mut at) = unleb128(bytes);
            let fields = program.fields(cons).to_vec();
            let mut elements = Vec::new();
            for _ in 0..count {
                let mut element = Vec::new();
                for (index, field) in fields.iter().enumerate() {
                    if index == recursive {
                        element.push(Value(nil, Vec::new()));
                    } else {
                        let (value, took) = decode(program, *field, &bytes[at..]);
                        element.push(value);
                        at += took;
                    }
                }
                elements.push(element);
            }
            // Built from the end, so each element holds the rest of the value.
            let mut value = Value(nil, Vec::new());
            for mut element in elements.into_iter().rev() {
                element[recursive] = value;
                value = Value(cons, element);
            }
            (value, at)
        }
    }
}

fn decode_tagged(program: &Program, id: TypeId, bytes: &[u8]) -> (Value, usize) {
    {
        {
            let mut at = 0;
            let ctor = if program.type_(id).ctors.len() > 1 {
                let (tag, took) = unleb128(bytes);
                at += took;
                program.ctor_at(id, u32::try_from(tag).expect("a tag within 4G"))
            } else {
                program.ctor_at(id, 0)
            };
            let mut fields = Vec::with_capacity(program.fields(ctor).len());
            for index in 0..program.fields(ctor).len() {
                let field = program.fields(ctor)[index];
                let (value, took) = decode(program, field, &bytes[at..]);
                fields.push(value);
                at += took;
            }
            (Value(ctor, fields), at)
        }
    }
}

/// A spine-shaped value whose elements carry nothing, which is a number.
/// Reading it costs nothing, where [`decode`] would build the whole chain.
///
/// # Panics
///
/// If the bytes are not an encoding of this type.
#[must_use]
pub fn as_number(program: &Program, id: TypeId, bytes: &[u8]) -> Option<u64> {
    let Shape::Spine { cons, .. } = shape(program, id) else {
        return None;
    };
    (program.fields(cons).len() == 1).then(|| unleb128(bytes).0)
}

/// The elements of a spine-shaped value, when each of them encoded to one
/// byte.
///
/// The run is then the value itself, so a target can lift it into static
/// memory as it stands rather than walking it element by element.
///
/// # Panics
///
/// If the bytes are not an encoding of this type.
#[must_use]
pub fn as_bytes<'a>(program: &Program, id: TypeId, bytes: &'a [u8]) -> Option<&'a [u8]> {
    if !matches!(shape(program, id), Shape::Spine { .. }) {
        return None;
    }
    let (count, at) = unleb128(bytes);
    let payload = &bytes[at..];
    let count = usize::try_from(count).expect("a count that fits a pointer");
    (payload.len() == count).then_some(payload)
}

/// Write a number straight into the pool, without building the chain of
/// constructors it stands for.
pub(super) fn write_number(value: u64, out: &mut Vec<u8>) {
    leb128(value, out);
}

/// The same for a run of bytes: the count, then each of them.
pub(super) fn write_bytes(bytes: &[u8], out: &mut Vec<u8>) {
    leb128(
        u64::try_from(bytes.len()).expect("a run within 64 bits"),
        out,
    );
    for byte in bytes {
        leb128(u64::from(*byte), out);
    }
}

/// Seven bits a byte, low first, with the high bit set while more follow.
fn leb128(mut value: u64, out: &mut Vec<u8>) {
    loop {
        let byte = u8::try_from(value & 0x7F).expect("seven bits");
        value >>= 7;
        if value == 0 {
            out.push(byte);
            return;
        }
        out.push(byte | 0x80);
    }
}

fn unleb128(bytes: &[u8]) -> (u64, usize) {
    let mut value = 0u64;
    for (at, byte) in bytes.iter().enumerate() {
        value |= u64::from(byte & 0x7F) << (7 * at);
        if byte & 0x80 == 0 {
            return (value, at + 1);
        }
    }
    panic!("a value that runs off the end of the pool")
}
