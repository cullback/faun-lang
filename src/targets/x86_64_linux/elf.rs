//! ELF header, program headers, then the segments they describe. No padding.
//!
//! Omitted, none of it consulted to execute a file:
//!
//! - Section headers, symbol table, notes, page alignment, dynamic linking.
//! - The cost is already due: `objdump -d` prints nothing and `nm` finds no
//!   symbols. Section headers and a `.symtab` go behind a flag when a
//!   disassembly is wanted, at roughly 290 bytes on that path.
//!
//! Position independence, at two bytes:
//!
//! - `lea rsi, [rip+d]` is 7 bytes against `mov esi, imm32`'s 5. `REX.W` is
//!   required; without it the address truncates to 32 bits.
//! - An absolute immediate needs a fixed load address, which puts the
//!   container format inside the instruction selector.
//! - Every address is a displacement within one mapping, so `ET_DYN` needs
//!   no `PT_DYNAMIC`, no relocation processing, and no interpreter.
//!
//! Size:
//!
//! - 120 of 158 bytes are header.
//! - Under about 140 needs code inside `e_shoff`, `e_flags`, or the
//!   `e_ident` padding. Rejected: unreadable, and the specification does not
//!   promise those fields stay unread.
//!
//! Segments come from a list. Two more are possible:
//!
//! - Read-only for the data, 56 bytes, no padding: two segments share a file
//!   page when `p_vaddr` matches `p_offset` modulo the page size.
//! - Writable, only for storage outliving every call. A heap comes from
//!   `mmap` and the stack from the kernel, so neither needs a segment.

use super::Code;
use crate::ir::{Origin, Program};
use crate::targets::bytes::{Bytes, len32};

pub(super) const EHDR_LEN: u16 = 64;
pub(super) const PHDR_LEN: u16 = 56;
const PAGE_SIZE: u64 = 0x1000;
const PAGE: usize = 0x1000;

/// `p_flags`.
const READ: u32 = 4;
const WRITE: u32 = 2;
const EXECUTE: u32 = 1;

/// One mapping the kernel makes.
struct Segment {
    flags: u32,
    start: u32,
    len: u32,
    /// Where it is mapped, which for everything but the writable region is
    /// its own file offset.
    vaddr: u32,
}

pub(super) fn image(code: &Code, program: &Program) -> Vec<u8> {
    let layout = Layout::new(code, program);
    let mut out = Bytes::default();

    header(&mut out, &layout);
    for segment in &layout.segments {
        program_header(&mut out, segment);
    }
    out.bytes(&relocated(code, &layout, program));
    out.bytes(program.data());
    out.bytes(program.globals());

    out.finish()
}

struct Layout {
    /// File offset of the code, and of the data that follows it.
    text: usize,
    data: usize,
    /// Where the globals are *mapped*, which is not where they sit in the
    /// file: they need a page of their own to be writable in.
    globals_at: usize,
    segments: Vec<Segment>,
}

impl Layout {
    fn new(code: &Code, program: &Program) -> Self {
        // What is only read shares one mapping with the code and the headers.
        // What is written needs its own, and only exists when something is.
        let writable = !program.globals().is_empty();
        let count = 1 + usize::from(writable);
        let text = usize::from(EHDR_LEN) + usize::from(PHDR_LEN) * count;
        let data = text + code.bytes.len();
        let globals = data + program.data().len();

        // A page further on. The two mappings then land on different pages
        // of memory while sharing one page of the file, which the kernel
        // allows so long as an address matches its offset modulo a page.
        let globals_at = globals + PAGE;

        let mut segments = vec![Segment {
            flags: READ | EXECUTE,
            start: 0,
            len: len32(globals),
            vaddr: 0,
        }];
        if writable {
            segments.push(Segment {
                flags: READ | WRITE,
                start: len32(globals),
                len: len32(program.globals().len()),
                vaddr: len32(globals_at),
            });
        }
        debug_assert_eq!(segments.len(), count);

        Self {
            text,
            data,
            globals_at,
            segments,
        }
    }
}

/// Each address as a displacement from the end of its own instruction, which
/// is why nothing here needs a load address.
fn relocated(code: &Code, layout: &Layout, program: &Program) -> Vec<u8> {
    let mut text = code.bytes.clone();
    for reloc in &code.relocs {
        let from = layout.text + reloc.offset + 4;
        let span = program.datum(reloc.data);
        let start = usize::try_from(span.start).expect("an offset in the image");
        // Addresses, not file offsets: the two happen to agree everywhere
        // but in the writable mapping, which sits a page further along.
        let to = match span.origin {
            Origin::Data => layout.data + start,
            Origin::Globals => layout.globals_at + start,
        };
        let displacement = to.abs_diff(from);
        let displacement = i32::try_from(displacement).expect("data within 2 GiB of the code");
        let displacement = if to < from {
            -displacement
        } else {
            displacement
        };
        text[reloc.offset..reloc.offset + 4].copy_from_slice(&displacement.to_le_bytes());
    }
    text
}

fn header(out: &mut Bytes, layout: &Layout) {
    out.bytes(b"\x7FELF"); // e_ident: magic
    out.byte(2); //             ELFCLASS64
    out.byte(1); //             ELFDATA2LSB
    out.byte(1); //             EV_CURRENT
    out.byte(0); //             ELFOSABI_SYSV
    out.byte(0); //             ABI version
    out.bytes(&[0; 7]); //      padding
    out.le16(3); // e_type: ET_DYN
    out.le16(0x3E); // e_machine: EM_X86_64
    out.le32(1); // e_version
    out.le64(u64::from(len32(layout.text))); // e_entry, before the load bias
    out.le64(u64::from(EHDR_LEN)); // e_phoff
    out.le64(0); // e_shoff
    out.le32(0); // e_flags
    out.le16(EHDR_LEN); // e_ehsize
    out.le16(PHDR_LEN); // e_phentsize
    out.le16(len16(layout.segments.len())); // e_phnum
    out.le16(0); // e_shentsize
    out.le16(0); // e_shnum
    out.le16(0); // e_shstrndx
}

/// A segment's address has to match its file offset modulo the page size,
/// which laying them out in order gives.
fn program_header(out: &mut Bytes, segment: &Segment) {
    out.le32(1); // p_type: PT_LOAD
    out.le32(segment.flags);
    out.le64(u64::from(segment.start)); // p_offset
    out.le64(u64::from(segment.vaddr)); // p_vaddr
    out.le64(u64::from(segment.vaddr)); // p_paddr
    out.le64(u64::from(segment.len)); // p_filesz
    out.le64(u64::from(segment.len)); // p_memsz
    out.le64(PAGE_SIZE); // p_align
}

fn len16(count: usize) -> u16 {
    u16::try_from(count).expect("a handful of segments")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::targets::Target;

    fn hello() -> Vec<u8> {
        crate::compile(Target::X86_64Linux, &crate::hello_world())
            .pop()
            .unwrap()
            .bytes
    }

    #[test]
    fn the_header_describes_a_loadable_executable() {
        let image = hello();

        assert_eq!(&image[..4], b"\x7FELF");
        assert_eq!(&image[16..18], &3u16.to_le_bytes()); // ET_DYN
        assert_eq!(&image[18..20], &0x3Eu16.to_le_bytes()); // EM_X86_64
        assert_eq!(&image[54..56], &56u16.to_le_bytes()); // e_phentsize
        assert_eq!(&image[56..58], &1u16.to_le_bytes()); // e_phnum

        // The segment covers the file exactly, so no byte is wasted.
        let filesz = u64::from_le_bytes(image[96..104].try_into().unwrap());
        assert_eq!(usize::try_from(filesz).unwrap(), image.len());
    }

    /// Nothing in the file names an address, which is what lets the kernel
    /// put it wherever it likes.
    #[test]
    fn the_message_is_reached_from_the_instruction_pointer() {
        let image = hello();
        let text = usize::from(EHDR_LEN + PHDR_LEN);
        assert_eq!(
            u64::from_le_bytes(image[24..32].try_into().unwrap()),
            u64::from(EHDR_LEN + PHDR_LEN),
            "the entry point is an offset, not an address"
        );

        // `lea rsi, [rip+disp]` starts five bytes into the code and puts its
        // displacement three bytes later, counted from its own end.
        let at = text + 8;
        let displacement = i32::from_le_bytes(image[at..at + 4].try_into().unwrap());
        let target = (at + 4).wrapping_add_signed(isize::try_from(displacement).unwrap());
        assert_eq!(&image[target..], b"Hello, World!\n");
    }
}
