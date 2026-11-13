//! MD5 (RFC 1321). sqllogictest stores long results as an MD5 of the
//! values, so the runner needs the digest; it is not used for security.

const S: [u32; 64] = [
    7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 7, 12, 17, 22, 5, 9, 14, 20, 5, 9, 14, 20, 5, 9,
    14, 20, 5, 9, 14, 20, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 4, 11, 16, 23, 6, 10, 15,
    21, 6, 10, 15, 21, 6, 10, 15, 21, 6, 10, 15, 21,
];

pub struct Md5 {
    state: [u32; 4],
    buf: Vec<u8>,
    len: u64,
    k: [u32; 64],
}

impl Default for Md5 {
    fn default() -> Self {
        let mut k = [0u32; 64];
        for (i, v) in k.iter_mut().enumerate() {
            *v = ((i as f64 + 1.0).sin().abs() * 4294967296.0) as u32;
        }
        Md5 {
            state: [0x67452301, 0xefcdab89, 0x98badcfe, 0x10325476],
            buf: Vec::new(),
            len: 0,
            k,
        }
    }
}

impl Md5 {
    fn block(&mut self, chunk: &[u8]) {
        let mut m = [0u32; 16];
        for (i, w) in m.iter_mut().enumerate() {
            *w = u32::from_le_bytes(chunk[i * 4..i * 4 + 4].try_into().unwrap());
        }
        let [mut a, mut b, mut c, mut d] = self.state;
        #[allow(clippy::needless_range_loop)]
        for i in 0..64 {
            let (f, g) = match i / 16 {
                0 => ((b & c) | (!b & d), i),
                1 => ((d & b) | (!d & c), (5 * i + 1) % 16),
                2 => (b ^ c ^ d, (3 * i + 5) % 16),
                _ => (c ^ (b | !d), (7 * i) % 16),
            };
            let f = f.wrapping_add(a).wrapping_add(self.k[i]).wrapping_add(m[g]);
            a = d;
            d = c;
            c = b;
            b = b.wrapping_add(f.rotate_left(S[i]));
        }
        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
    }

    pub fn update(&mut self, data: &[u8]) {
        self.len += data.len() as u64;
        self.buf.extend_from_slice(data);
        let full = self.buf.len() / 64 * 64;
        let buf = std::mem::take(&mut self.buf);
        for chunk in buf[..full].chunks_exact(64) {
            self.block(chunk);
        }
        self.buf = buf[full..].to_vec();
    }

    pub fn hex(mut self) -> String {
        let bits = self.len.wrapping_mul(8);
        let mut tail = std::mem::take(&mut self.buf);
        tail.push(0x80);
        while tail.len() % 64 != 56 {
            tail.push(0);
        }
        tail.extend_from_slice(&bits.to_le_bytes());
        for chunk in tail.chunks_exact(64) {
            self.block(chunk);
        }
        self.state
            .iter()
            .flat_map(|w| w.to_le_bytes())
            .map(|b| format!("{b:02x}"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::Md5;

    fn md5(s: &str) -> String {
        let mut m = Md5::default();
        m.update(s.as_bytes());
        m.hex()
    }

    #[test]
    fn rfc1321_vectors() {
        assert_eq!(md5(""), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(md5("abc"), "900150983cd24fb0d6963f7d28e17f72");
        assert_eq!(md5("message digest"), "f96b697d7cb7938d525a2f31aaf161d0");
        assert_eq!(
            md5("12345678901234567890123456789012345678901234567890123456789012345678901234567890"),
            "57edf4a22be3c955ac49da2e2107b67a"
        );
    }
}
