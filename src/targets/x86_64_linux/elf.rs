//! Three pieces end to end with no padding: the ELF header, one program
//! header, and a single read-execute segment covering the whole file. Section
//! headers, symbols, alignment and notes are all optional for execution, so
//! none of them are here. That is the whole trick behind the size.

use super::Code;
use crate::targets::bytes::{Bytes, len32};

/// Where the single segment is mapped. Must sit at or above `mmap_min_addr`
/// (64 KiB by default) and match the file offset modulo the page size.
pub(super) const LOAD_ADDR: u32 = 0x1_0000;

pub(super) const EHDR_LEN: u16 = 64;
pub(super) const PHDR_LEN: u16 = 56;
const PAGE_SIZE: u64 = 0x1000;

pub(super) fn image(code: &Code, data: &[Vec<u8>]) -> Vec<u8> {
    let text_offset = EHDR_LEN + PHDR_LEN;
    let data_offset = usize::from(text_offset) + code.bytes.len();

    let mut addrs = Vec::with_capacity(data.len());
    let mut blob = Vec::new();
    for bytes in data {
        addrs.push(data_offset + blob.len());
        blob.extend_from_slice(bytes);
    }

    let mut text = code.bytes.clone();
    for reloc in &code.relocs {
        let addr = LOAD_ADDR
            .checked_add(len32(addrs[reloc.data.0]))
            .expect("the image fits in the 4 GiB a 32-bit address reaches");
        text[reloc.offset..reloc.offset + 4].copy_from_slice(&addr.to_le_bytes());
    }

    let total = len32(data_offset + blob.len());
    let entry = u64::from(LOAD_ADDR) + u64::from(text_offset);
    let mut out = Bytes::default();

    // Elf64_Ehdr.
    out.bytes(b"\x7FELF"); // e_ident: magic
    out.byte(2); //             ELFCLASS64
    out.byte(1); //             ELFDATA2LSB
    out.byte(1); //             EV_CURRENT
    out.byte(0); //             ELFOSABI_SYSV
    out.byte(0); //             ABI version
    out.bytes(&[0; 7]); //      padding
    out.le16(2); // e_type: ET_EXEC
    out.le16(0x3E); // e_machine: EM_X86_64
    out.le32(1); // e_version
    out.le64(entry); // e_entry
    out.le64(u64::from(EHDR_LEN)); // e_phoff
    out.le64(0); // e_shoff
    out.le32(0); // e_flags
    out.le16(EHDR_LEN); // e_ehsize
    out.le16(PHDR_LEN); // e_phentsize
    out.le16(1); // e_phnum
    out.le16(0); // e_shentsize
    out.le16(0); // e_shnum
    out.le16(0); // e_shstrndx

    // Elf64_Phdr: one segment, its own headers included.
    out.le32(1); // p_type: PT_LOAD
    out.le32(5); // p_flags: PF_R | PF_X
    out.le64(0); // p_offset
    out.le64(u64::from(LOAD_ADDR)); // p_vaddr
    out.le64(u64::from(LOAD_ADDR)); // p_paddr
    out.le64(u64::from(total)); // p_filesz
    out.le64(u64::from(total)); // p_memsz
    out.le64(PAGE_SIZE); // p_align

    out.bytes(&text);
    out.bytes(&blob);

    let image = out.finish();
    debug_assert_eq!(len32(image.len()), total);
    image
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
        assert_eq!(&image[16..18], &2u16.to_le_bytes()); // ET_EXEC
        assert_eq!(&image[18..20], &0x3Eu16.to_le_bytes()); // EM_X86_64
        assert_eq!(&image[54..56], &56u16.to_le_bytes()); // e_phentsize
        assert_eq!(&image[56..58], &1u16.to_le_bytes()); // e_phnum

        // The segment covers the file exactly, so no byte is wasted.
        let filesz = u64::from_le_bytes(image[96..104].try_into().unwrap());
        assert_eq!(usize::try_from(filesz).unwrap(), image.len());
    }

    #[test]
    fn the_message_address_is_resolved() {
        let image = hello();

        let entry = u64::from_le_bytes(image[24..32].try_into().unwrap());
        assert_eq!(entry, u64::from(LOAD_ADDR) + u64::from(EHDR_LEN + PHDR_LEN));

        // `mov esi, imm32` sits six bytes into the code, and its immediate
        // must point at the message the layout placed after the code.
        let text = usize::from(EHDR_LEN + PHDR_LEN);
        let addr = u32::from_le_bytes(image[text + 6..text + 10].try_into().unwrap());
        let offset = usize::try_from(addr - LOAD_ADDR).unwrap();
        assert_eq!(&image[offset..], b"Hello, World!\n");
    }
}
