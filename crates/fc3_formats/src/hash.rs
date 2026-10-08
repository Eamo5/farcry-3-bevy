//! Hash functions used by the Dunia 2 engine.
//!
//! * File names inside FAT archives are hashed with a reflected CRC-64
//!   (polynomial 0xD800000000000000, zero init, no final xor) over the
//!   lower-cased path.
//! * Binary object (FCB) class and field names use the standard zlib CRC-32.

const fn crc64_table() -> [u64; 256] {
    let mut table = [0u64; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u64;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xD800_0000_0000_0000 } else { crc >> 1 };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

const fn crc32_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = i as u32;
        let mut j = 0;
        while j < 8 {
            crc = if crc & 1 != 0 { (crc >> 1) ^ 0xEDB8_8320 } else { crc >> 1 };
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
}

static CRC64_TABLE: [u64; 256] = crc64_table();
static CRC32_TABLE: [u32; 256] = crc32_table();

pub fn crc64(bytes: &[u8]) -> u64 {
    let mut hash = 0u64;
    for &b in bytes {
        hash = CRC64_TABLE[((hash as u8) ^ b) as usize] ^ (hash >> 8);
    }
    hash
}

pub fn crc32(bytes: &[u8]) -> u32 {
    let mut hash = 0xFFFF_FFFFu32;
    for &b in bytes {
        hash = CRC32_TABLE[((hash as u8) ^ b) as usize] ^ (hash >> 8);
    }
    !hash
}

/// Hash an archive path the way the engine does: lower-case, backslash separators.
pub fn path_hash(path: &str) -> u64 {
    let normalized: String = path
        .chars()
        .map(|c| if c == '/' { '\\' } else { c.to_ascii_lowercase() })
        .collect();
    crc64(normalized.as_bytes())
}

/// CRC-32 of a binary-object class/field name (case sensitive).
pub fn name_hash(name: &str) -> u32 {
    crc32(name.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tables_match_reference() {
        assert_eq!(CRC64_TABLE[1], 0x01B0_0000_0000_0000);
        assert_eq!(CRC64_TABLE[255], 0x9090_0000_0000_0000);
        assert_eq!(CRC32_TABLE[1], 0x7707_3096);
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
