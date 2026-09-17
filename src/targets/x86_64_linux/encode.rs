//! Each method appends exactly one instruction. Choosing between two
//! encodings of the same value is [`super::lower`]'s job, not this module's.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub(super) enum Reg {
    Rax = 0,
    Rcx = 1,
    Rdx = 2,
    Rsi = 6,
    Rdi = 7,
    R11 = 11,
}

impl From<Reg> for u8 {
    #[expect(clippy::as_conversions, reason = "reading a repr(u8) discriminant")]
    fn from(reg: Reg) -> Self {
        reg as Self
    }
}

impl Reg {
    pub(super) fn index(self) -> usize {
        usize::from(u8::from(self))
    }

    /// The low three bits, which is all `ModRM` and `+r` have room for.
    fn low(self) -> u8 {
        u8::from(self) & 7
    }

    fn extended(self) -> bool {
        u8::from(self) >= 8
    }
}

#[derive(Default)]
pub(super) struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    pub(super) fn finish(self) -> Vec<u8> {
        self.bytes
    }

    /// The shortest zeroing, and it clears all 64 bits despite naming the
    /// 32-bit half.
    pub(super) fn xor_self(&mut self, dst: Reg) {
        self.rex(false, Some(dst), dst);
        self.emit(&[0x31, modrm(dst, dst)]);
    }

    pub(super) fn mov_reg(&mut self, dst: Reg, src: Reg, wide: bool) {
        self.rex(wide, Some(src), dst);
        self.emit(&[0x89, modrm(src, dst)]);
    }

    /// Sign-extends to a full 64-bit stack slot.
    pub(super) fn push_imm8(&mut self, value: i8) {
        self.emit(&[0x6A, value.cast_unsigned()]);
    }

    pub(super) fn pop(&mut self, dst: Reg) {
        self.rex(false, None, dst);
        self.emit(&[0x58 + dst.low()]);
    }

    /// Zero-extends to 64 bits. Returns the offset of the immediate, which
    /// is where a relocation would land.
    pub(super) fn mov_imm32(&mut self, dst: Reg, value: u32) -> usize {
        self.rex(false, None, dst);
        self.emit(&[0xB8 + dst.low()]);
        let offset = self.bytes.len();
        self.emit(&value.to_le_bytes());
        offset
    }

    /// Sign-extended to 64 bits, for negative constants.
    pub(super) fn mov_sext_imm32(&mut self, dst: Reg, value: i32) {
        // ModRM.reg holds the opcode extension /0, not a register.
        self.rex(true, None, dst);
        self.emit(&[0xC7, modrm(Reg::Rax, dst)]);
        self.emit(&value.to_le_bytes());
    }

    /// The only form that carries a full 64-bit value.
    pub(super) fn mov_imm64(&mut self, dst: Reg, value: u64) {
        self.emit(&[0x48 | u8::from(dst.extended()), 0xB8 + dst.low()]);
        self.emit(&value.to_le_bytes());
    }

    pub(super) fn syscall(&mut self) {
        self.emit(&[0x0F, 0x05]);
    }

    /// `dst = [rsp + disp]`, and its mirror. The `ModRM` `rm` of 100 means a
    /// SIB byte follows; 0x24 is rsp as the base with no index.
    pub(super) fn load_slot(&mut self, dst: Reg, disp: u32) {
        self.slot(0x8B, dst, disp);
    }

    pub(super) fn store_slot(&mut self, src: Reg, disp: u32) {
        self.slot(0x89, src, disp);
    }

    fn slot(&mut self, opcode: u8, reg: Reg, disp: u32) {
        self.emit(&[0x48 | (u8::from(reg.extended()) << 2), opcode]);
        self.emit(&[0x80 | (reg.low() << 3) | 4, 0x24]);
        self.emit(&disp.to_le_bytes());
    }

    /// `sub rsp, bytes`, to open the frame the slots live in.
    pub(super) fn open_frame(&mut self, bytes: u32) {
        self.emit(&[0x48, 0x81, 0xEC]);
        self.emit(&bytes.to_le_bytes());
    }

    /// `dst = dst op src` for the one-byte ALU forms.
    pub(super) fn alu(&mut self, opcode: u8, dst: Reg, src: Reg) {
        self.rex(true, Some(src), dst);
        self.emit(&[opcode, modrm(src, dst)]);
    }

    /// `dst = 1` when the flags satisfy `cc`, else 0, zero-extended.
    pub(super) fn set_if(&mut self, cc: u8, dst: Reg) {
        self.rex(false, None, dst);
        self.emit(&[0x0F, 0x90 | cc, 0xC0 | dst.low()]);
        self.rex(false, Some(dst), dst);
        self.emit(&[0x0F, 0xB6, modrm(dst, dst)]);
    }

    /// `dst = rip + disp`, the address of something a known distance away.
    /// Returns the offset of the displacement, which is measured from the
    /// end of this instruction. `REX.W` is not optional: without it the
    /// address is truncated to 32 bits, which ASLR makes fatal.
    pub(super) fn lea_rip(&mut self, dst: Reg) -> usize {
        self.rex(true, Some(dst), Reg::Rax);
        self.emit(&[0x8D, (dst.low() << 3) | 5]);
        self.hole()
    }

    /// An unconditional jump, or one taken when the flags satisfy `cc`.
    /// Both leave a hole for [`Encoder::patch`], whose offset is returned.
    pub(super) fn jump(&mut self) -> usize {
        self.emit(&[0xE9]);
        self.hole()
    }

    pub(super) fn jump_if(&mut self, cc: u8) -> usize {
        self.emit(&[0x0F, 0x80 | cc]);
        self.hole()
    }

    fn hole(&mut self) -> usize {
        let at = self.bytes.len();
        self.emit(&[0; 4]);
        at
    }

    pub(super) const fn here(&self) -> usize {
        self.bytes.len()
    }

    /// Aim a jump left by [`Encoder::jump`] at `target`. Displacements count
    /// from the end of the instruction, which is the end of the hole.
    pub(super) fn patch(&mut self, hole: usize, target: usize) {
        let from = i64::try_from(hole + 4).expect("a reachable offset");
        let to = i64::try_from(target).expect("a reachable offset");
        let displacement = i32::try_from(to - from).expect("a jump within 2 GiB");
        self.bytes[hole..hole + 4].copy_from_slice(&displacement.to_le_bytes());
    }

    /// `reg` is `None` for the forms with no `ModRM` byte. Omitted entirely
    /// when every bit would be zero.
    fn rex(&mut self, wide: bool, reg: Option<Reg>, rm: Reg) {
        let reg = reg.is_some_and(Reg::extended);
        let bits = (u8::from(wide) << 3) | (u8::from(reg) << 2) | u8::from(rm.extended());
        if bits != 0 {
            self.emit(&[0x40 | bits]);
        }
    }

    fn emit(&mut self, bytes: &[u8]) {
        self.bytes.extend_from_slice(bytes);
    }
}

fn modrm(reg: Reg, rm: Reg) -> u8 {
    0xC0 | (reg.low() << 3) | rm.low()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encoded(build: impl FnOnce(&mut Encoder)) -> Vec<u8> {
        let mut asm = Encoder::default();
        build(&mut asm);
        asm.finish()
    }

    #[test]
    fn each_form_encodes_to_its_documented_bytes() {
        assert_eq!(encoded(|a| a.xor_self(Reg::Rdi)), [0x31, 0xFF]);
        assert_eq!(encoded(|a| a.pop(Reg::Rdx)), [0x5A]);
        assert_eq!(encoded(|a| a.push_imm8(-5)), [0x6A, 0xFB]);
        assert_eq!(
            encoded(|a| a.mov_reg(Reg::Rdi, Reg::Rax, false)),
            [0x89, 0xC7]
        );
        assert_eq!(
            encoded(|a| a.mov_reg(Reg::Rdx, Reg::Rax, true)),
            [0x48, 0x89, 0xC2]
        );
        #[rustfmt::skip]
        assert_eq!(encoded(|a| a.mov_sext_imm32(Reg::Rdi, -1)),
            [0x48, 0xC7, 0xC7, 0xFF, 0xFF, 0xFF, 0xFF]);
        #[rustfmt::skip]
        assert_eq!(encoded(|a| a.mov_imm64(Reg::Rdx, 0x7FFF_FFFF_FFFF_FFFF)),
            [0x48, 0xBA, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x7F]);
    }

    #[test]
    fn high_registers_carry_rex_b() {
        assert_eq!(encoded(|a| a.pop(Reg::R11)), [0x41, 0x5B]);
        assert_eq!(encoded(|a| a.xor_self(Reg::R11)), [0x45, 0x31, 0xDB]);
    }
}
