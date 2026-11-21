//! The identifiers bd derives, ported so a row lc-serve writes carries the id bd would give it:
//! the hash id of a created bead, a dependency edge's primary key, the content hash, and the
//! coordination cell a parent-child write rewrites.

use sha2::{Digest, Sha256};

const BASE36: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";

fn encode_base36(data: &[u8], length: usize) -> String {
    let mut num = data.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b));
    let mut digits = Vec::new();
    while num > 0 {
        digits.push(BASE36[(num % 36) as usize]);
        num /= 36;
    }
    digits.reverse();
    let text = String::from_utf8(digits).unwrap_or_default();
    match text.len().cmp(&length) {
        std::cmp::Ordering::Less => format!("{}{text}", "0".repeat(length - text.len())),
        std::cmp::Ordering::Greater => text[text.len() - length..].to_string(),
        std::cmp::Ordering::Equal => text,
    }
}

/// bd's `GenerateHashID`: the digest of `title|description|creator|unix-nanos|nonce`, cut to
/// the bytes `length` calls for and written in base36.
pub fn hash_id(prefix: &str, title: &str, description: &str, creator: &str, unix_nanos: i128, length: usize, nonce: usize) -> String {
    let digest = Sha256::digest(format!("{title}|{description}|{creator}|{unix_nanos}|{nonce}").as_bytes());
    let bytes = match length {
        3 => 2,
        4 => 3,
        5 | 6 => 4,
        7 | 8 => 5,
        _ => 3,
    };
    format!("{prefix}-{}", encode_base36(&digest[..bytes], length))
}

/// bd's `ComputeAdaptiveLength`: the shortest length whose collision probability over `count`
/// beads is within `max_probability`.
pub fn adaptive_length(count: u64, max_probability: f64, min: usize, max: usize) -> usize {
    (min..=max)
        .find(|len| {
            let possibilities = 36f64.powi(*len as i32);
            1.0 - (-((count * count) as f64) / (2.0 * possibilities)).exp() <= max_probability
        })
        .unwrap_or(max)
}

fn sha1(data: &[u8]) -> [u8; 20] {
    let mut h: [u32; 5] = [0x6745_2301, 0xEFCD_AB89, 0x98BA_DCFE, 0x1032_5476, 0xC3D2_E1F0];
    let mut msg = data.to_vec();
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&((data.len() as u64) * 8).to_be_bytes());
    for block in msg.chunks(64) {
        let mut w = [0u32; 80];
        for (i, word) in block.chunks(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let [mut a, mut b, mut c, mut d, mut e] = h;
        for (i, wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | (!b & d), 0x5A82_7999),
                20..=39 => (b ^ c ^ d, 0x6ED9_EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1B_BCDC),
                _ => (b ^ c ^ d, 0xCA62_C1D6u32),
            };
            let t = a.rotate_left(5).wrapping_add(f).wrapping_add(e).wrapping_add(k).wrapping_add(*wi);
            (e, d, c, b, a) = (d, c, b.rotate_left(30), a, t);
        }
        for (slot, v) in h.iter_mut().zip([a, b, c, d, e]) {
            *slot = slot.wrapping_add(v);
        }
    }
    let mut out = [0u8; 20];
    for (chunk, word) in out.chunks_mut(4).zip(h) {
        chunk.copy_from_slice(&word.to_be_bytes());
    }
    out
}

/// bd's `depid.New`: the RFC 4122 version-5 UUID of `issue 0x1f target` in bd's dependency
/// namespace, so an edge has the same primary key whoever writes it.
pub fn dependency_id(issue: &str, target: &str) -> String {
    const NAMESPACE: [u8; 16] = [0xbd, 0xd7, 0x4e, 0xb9, 0xa2, 0x0a, 0x55, 0x4a, 0x98, 0x5b, 0x54, 0xb8, 0xf3, 0xe6, 0x4d, 0x8b];
    let mut name = NAMESPACE.to_vec();
    name.extend_from_slice(format!("{issue}\u{1f}{target}").as_bytes());
    let mut b = [0u8; 16];
    b.copy_from_slice(&sha1(&name)[..16]);
    b[6] = (b[6] & 0x0f) | 0x50;
    b[8] = (b[8] & 0x3f) | 0x80;
    let hex: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..])
}

/// The fields of a new bead that bd's `ComputeContentHash` reads, for a bead made by `create`
/// (every field the filing flags cannot set is empty).
pub struct Content<'a> {
    pub title: &'a str,
    pub description: &'a str,
    pub status: &'a str,
    pub priority: u8,
    pub issue_type: &'a str,
    pub assignee: &'a str,
    pub owner: &'a str,
    pub created_by: &'a str,
    pub external_ref: &'a str,
}

/// bd's content hash: each field followed by a NUL, in bd's order.
pub fn content_hash(c: &Content) -> String {
    let priority = c.priority.to_string();
    let fields = [c.title, c.description, "", "", "", "", c.status, &priority, c.issue_type, c.assignee, c.owner, c.created_by, c.external_ref, "", "", "", "", "", "", "0", "", "", "", "", "", ""];
    let mut h = Sha256::new();
    for f in fields {
        h.update(f.as_bytes());
        h.update([0]);
    }
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// The `local_metadata` key a parent's dependency writes rewrite in `tier`, so two transactions
/// that touch one parent's edges conflict instead of merging.
pub fn coordination_key(parent: &str, tier: &str) -> String {
    let sum = Sha256::digest(parent.as_bytes());
    let shard = (u16::from(sum[0]) << 4) | (u16::from(sum[1]) >> 4);
    format!("dependency-coordination/v1/{tier}/{shard:03x}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hash_id_is_the_one_bd_gives_the_same_inputs() {
        let nanos = 1_704_164_645_006_000_000i128;
        for (len, want) in [(3, "bd-vju"), (4, "bd-8d8e"), (5, "bd-bi3tk"), (6, "bd-8bi3tk"), (7, "bd-r5sr6bm"), (8, "bd-8r5sr6bm")] {
            assert_eq!(hash_id("bd", "Fix login", "Details", "jira-import", nanos, len, 0), want, "length {len}");
        }
    }

    #[test]
    fn a_dependency_id_is_the_one_bd_gives_the_same_edge() {
        for (issue, target, want) in [
            ("issue-1", "issue-2", "91990ee3-275c-59c5-9291-30f8a8732228"),
            ("mybd-7x76", "mybd-kd8a", "c77e0366-7f45-5f8c-9f12-2c51e0a0c5fd"),
            ("a", "external:https://x/y", "0ac7aa98-536c-5bfa-9e11-df69cc67438a"),
            ("w-1", "wisp-2", "a98c6757-0779-51f0-8fc1-8d2c8be7cb97"),
        ] {
            assert_eq!(dependency_id(issue, target), want);
        }
    }

    #[test]
    fn sha1_matches_the_standard_vectors() {
        let hex = |d: [u8; 20]| d.iter().map(|b| format!("{b:02x}")).collect::<String>();
        assert_eq!(hex(sha1(b"abc")), "a9993e364706816aba3e25717850c26c9cd0d89d");
        assert_eq!(hex(sha1(b"")), "da39a3ee5e6b4b0d3255bfef95601890afd80709");
        assert_eq!(hex(sha1(b"The quick brown fox jumps over the lazy dog")), "2fd4e1c67a2d28fced849ee1bb76e7391b93eb12");
        assert_eq!(hex(sha1(&vec![b'a'; 1_000_000])), "34aa973cd4c4daa4f61eeb2bdbad27316534016f");
    }

    #[test]
    fn the_id_grows_with_the_store_the_way_bd_grows_it() {
        assert_eq!(adaptive_length(0, 0.25, 3, 8), 3);
        assert_eq!(adaptive_length(100, 0.25, 3, 8), 3);
        assert_eq!(adaptive_length(300, 0.25, 3, 8), 4);
        assert_eq!(adaptive_length(10_000, 0.25, 3, 8), 6);
        assert_eq!(adaptive_length(u32::MAX as u64, 0.25, 3, 8), 8);
    }

    #[test]
    fn a_short_encoding_is_zero_padded_and_a_long_one_keeps_its_low_digits() {
        assert_eq!(encode_base36(&[0, 1], 3), "001");
        assert_eq!(encode_base36(&[0xff, 0xff], 3), "ekf");
        assert_eq!(encode_base36(&[], 3), "000");
    }

    #[test]
    fn the_coordination_cell_is_one_of_4096_per_tier() {
        let key = coordination_key("sp-parent", "dependencies");
        assert!(key.starts_with("dependency-coordination/v1/dependencies/"), "{key}");
        assert_eq!(key.rsplit('/').next().unwrap().len(), 3);
        assert_eq!(key, coordination_key("sp-parent", "dependencies"));
    }
}
