/// Both formats hold every length and index in 32 bits.
pub(super) fn len32(len: usize) -> u32 {
    u32::try_from(len).expect("the output fits in 32 bits")
}

#[derive(Debug, Default)]
pub(super) struct Bytes(Vec<u8>);

impl Bytes {
    pub(super) fn finish(self) -> Vec<u8> {
        self.0
    }

    pub(super) fn byte(&mut self, value: u8) {
        self.0.push(value);
    }

    pub(super) fn bytes(&mut self, value: &[u8]) {
        self.0.extend_from_slice(value);
    }

    pub(super) fn le16(&mut self, value: u16) {
        self.bytes(&value.to_le_bytes());
    }

    pub(super) fn le32(&mut self, value: u32) {
        self.bytes(&value.to_le_bytes());
    }

    pub(super) fn le64(&mut self, value: u64) {
        self.bytes(&value.to_le_bytes());
    }

    pub(super) fn uleb(&mut self, mut value: u32) {
        while value >= 0x80 {
            self.byte(value.to_le_bytes()[0] & 0x7F | 0x80);
            value >>= 7;
        }
        self.byte(value.to_le_bytes()[0]);
    }

    /// A byte is the last only once the bits left over agree with the sign
    /// bit it just wrote.
    pub(super) fn sleb(&mut self, mut value: i32) {
        loop {
            let byte = value.to_le_bytes()[0] & 0x7F;
            value >>= 7;
            let signed = byte & 0x40 != 0;
            if (value == 0 && !signed) || (value == -1 && signed) {
                return self.byte(byte);
            }
            self.byte(byte | 0x80);
        }
    }

    pub(super) fn name(&mut self, text: &str) {
        self.uleb(len32(text.len()));
        self.bytes(text.as_bytes());
    }

    /// Whatever `build` writes, prefixed by its length.
    pub(super) fn sized(&mut self, build: impl FnOnce(&mut Self)) {
        let mut body = Self::default();
        build(&mut body);
        self.uleb(len32(body.0.len()));
        self.bytes(&body.0);
    }

    /// A wasm section. One that would come out empty is left out entirely.
    pub(super) fn section(&mut self, id: u8, build: impl FnOnce(&mut Self)) {
        let mut body = Self::default();
        build(&mut body);
        if !body.0.is_empty() {
            self.byte(id);
            self.sized(|out| out.bytes(&body.0));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn written(build: impl FnOnce(&mut Bytes)) -> Vec<u8> {
        let mut bytes = Bytes::default();
        build(&mut bytes);
        bytes.finish()
    }

    #[test]
    fn unsigned_integers_grow_a_byte_at_a_time() {
        assert_eq!(written(|b| b.uleb(0)), [0x00]);
        assert_eq!(written(|b| b.uleb(127)), [0x7F]);
        assert_eq!(written(|b| b.uleb(128)), [0x80, 0x01]);
        assert_eq!(written(|b| b.uleb(624_485)), [0xE5, 0x8E, 0x26]);
    }

    #[test]
    fn signed_integers_carry_their_sign() {
        assert_eq!(written(|b| b.sleb(0)), [0x00]);
        assert_eq!(written(|b| b.sleb(63)), [0x3F]);
        // 64 needs a second byte, because 0x40 would read back as -64.
        assert_eq!(written(|b| b.sleb(64)), [0xC0, 0x00]);
        assert_eq!(written(|b| b.sleb(-1)), [0x7F]);
        assert_eq!(written(|b| b.sleb(-123_456)), [0xC0, 0xBB, 0x78]);
    }

    #[test]
    fn an_empty_section_is_left_out_entirely() {
        assert_eq!(written(|b| b.section(11, |_| {})), []);
        assert_eq!(written(|b| b.section(11, |s| s.byte(9))), [11, 1, 9]);
    }
}
