/// 38-byte ASCII RC4 key extracted from DeskGo 3.0.1409.127 Features64.dll (FUN_1802df060).
pub const DESKGO_RC4_KEY: &[u8] = b"{8CEFC9E6-A2B4-4c2a-823C-6903A31139FA}";

/// Standard RC4 (KSA + PRGA). Symmetric encryption/decryption with no IV or salt.
pub fn rc4_apply(key: &[u8], data: &[u8]) -> Vec<u8> {
    assert!(!key.is_empty(), "RC4 key must not be empty");
    let mut s = [0u8; 256];
    for (i, v) in s.iter_mut().enumerate() {
        *v = i as u8;
    }
    let mut j: u8 = 0;
    for i in 0..256 {
        j = j.wrapping_add(s[i]).wrapping_add(key[i % key.len()]);
        s.swap(i, j as usize);
    }
    let mut out = Vec::with_capacity(data.len());
    let mut i: u8 = 0;
    let mut j: u8 = 0;
    for &b in data {
        i = i.wrapping_add(1);
        j = j.wrapping_add(s[i as usize]);
        s.swap(i as usize, j as usize);
        let k = s[(s[i as usize].wrapping_add(s[j as usize])) as usize];
        out.push(b ^ k);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rc4_is_symmetric() {
        let plain = b"hello<|>world\n\n\0";
        let cipher = rc4_apply(DESKGO_RC4_KEY, plain);
        assert_ne!(cipher, plain);
        assert_eq!(rc4_apply(DESKGO_RC4_KEY, &cipher), plain);
    }
}
