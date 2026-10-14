use sha2::{Digest, Sha512};
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

const ALPHABET: &[u8; 64] = b"./0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const DEFAULT_ROUNDS: usize = 5000;

pub fn sha512_crypt(pw: &[u8], salt: &[u8], rounds: usize) -> String {
    let salt = &salt[..salt.len().min(16)];
    let b = Sha512::new().chain_update(pw).chain_update(salt).chain_update(pw).finalize();

    let mut a = Sha512::new().chain_update(pw).chain_update(salt);
    let mut n = pw.len();
    while n > 0 {
        a.update(&b[..n.min(64)]);
        n = n.saturating_sub(64);
    }
    let mut bits = pw.len();
    while bits > 0 {
        if bits & 1 == 1 {
            a.update(b);
        } else {
            a.update(pw);
        }
        bits >>= 1;
    }
    let mut digest = a.finalize();

    let mut dp = Sha512::new();
    for _ in 0..pw.len() {
        dp.update(pw);
    }
    let p = repeat_to(&dp.finalize(), pw.len());
    let mut ds = Sha512::new();
    for _ in 0..(16 + digest[0] as usize) {
        ds.update(salt);
    }
    let s = repeat_to(&ds.finalize(), salt.len());

    for i in 0..rounds {
        let mut c = Sha512::new();
        if i & 1 == 1 {
            c.update(&p);
        } else {
            c.update(digest);
        }
        if i % 3 != 0 {
            c.update(&s);
        }
        if i % 7 != 0 {
            c.update(&p);
        }
        if i & 1 == 1 {
            c.update(digest);
        } else {
            c.update(&p);
        }
        digest = c.finalize();
    }

    const ORDER: [(usize, usize, usize); 21] = [
        (0, 21, 42), (22, 43, 1), (44, 2, 23), (3, 24, 45), (25, 46, 4), (47, 5, 26), (6, 27, 48),
        (28, 49, 7), (50, 8, 29), (9, 30, 51), (31, 52, 10), (53, 11, 32), (12, 33, 54), (34, 55, 13),
        (56, 14, 35), (15, 36, 57), (37, 58, 16), (59, 17, 38), (18, 39, 60), (40, 61, 19), (62, 20, 41),
    ];
    let mut out = String::new();
    let mut emit = |w: u32, chars: usize| {
        let mut w = w;
        for _ in 0..chars {
            out.push(ALPHABET[(w & 0x3f) as usize] as char);
            w >>= 6;
        }
    };
    for (x, y, z) in ORDER {
        emit(((digest[x] as u32) << 16) | ((digest[y] as u32) << 8) | digest[z] as u32, 4);
    }
    emit(digest[63] as u32, 2);
    let salt = String::from_utf8_lossy(salt);
    if rounds == DEFAULT_ROUNDS {
        format!("$6${salt}${out}")
    } else {
        format!("$6$rounds={rounds}${salt}${out}")
    }
}

fn repeat_to(block: &[u8], len: usize) -> Vec<u8> {
    block.iter().cycle().take(len).copied().collect()
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut d = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().max(b.len()) {
        d |= a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0);
    }
    d == 0
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Credential {
    Plain(String),
    Sha512Crypt(String),
}

impl Credential {
    pub fn parse(text: &str, user: &str) -> Result<Credential, String> {
        let prefix = format!("{user}:");
        let line = text
            .lines()
            .map(str::trim)
            .find(|l| !l.is_empty() && !l.starts_with('#'))
            .ok_or("the password file is empty")?;
        let value = match line.strip_prefix(&prefix) {
            Some(rest) => rest.split(':').next().unwrap_or(""),
            None => line,
        };
        let hash = value.strip_prefix("{SHA512-CRYPT}").unwrap_or(value);
        if hash.starts_with("$6$") {
            let parts: Vec<&str> = hash.split('$').collect();
            let ok = parts.len() == 4 || (parts.len() == 5 && parts[2].starts_with("rounds="));
            if !ok {
                return Err("the password file holds a malformed $6$ hash".into());
            }
            return Ok(Credential::Sha512Crypt(hash.to_string()));
        }
        if value.is_empty() {
            return Err("the password file holds an empty password".into());
        }
        Ok(Credential::Plain(value.to_string()))
    }

    pub fn load(path: &Path, user: &str) -> Result<Credential, String> {
        let mode = std::fs::metadata(path).map_err(|e| format!("{}: {e}", path.display()))?.permissions().mode();
        if mode & 0o077 != 0 {
            return Err(format!("{} is mode {:o}; it must be 0600 (group/other bits set)", path.display(), mode & 0o777));
        }
        let text = std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?;
        Credential::parse(&text, user)
    }

    pub fn verify(&self, password: &str) -> bool {
        match self {
            Credential::Plain(p) => ct_eq(p.as_bytes(), password.as_bytes()),
            Credential::Sha512Crypt(h) => {
                let parts: Vec<&str> = h.split('$').collect();
                let (rounds, salt) = if parts.len() == 5 {
                    match parts[2].strip_prefix("rounds=").and_then(|r| r.parse::<usize>().ok()) {
                        Some(r) => (r.clamp(1000, 999_999_999), parts[3]),
                        None => return false,
                    }
                } else {
                    (DEFAULT_ROUNDS, parts[2])
                };
                let want = sha512_crypt(password.as_bytes(), salt.as_bytes(), rounds);
                ct_eq(want.rsplit('$').next().unwrap_or("").as_bytes(), h.rsplit('$').next().unwrap_or("x").as_bytes())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const HASH: &str = "$6$abcdefgh$yIZAF3gQPvtKZO/9qOJKffAKKbtS3ef3qmwyugk4uWVjX8YZf/GV3A8SkFxEPY0T56CcilGrHKLffBsp6dLMG.";

    #[test]
    fn matches_the_reference_openssl_hash() {
        assert_eq!(sha512_crypt(b"correct horse", b"abcdefgh", 5000), HASH);
        assert_eq!(
            sha512_crypt(b"p", b"xxxxxxxxxxxxxxxx", 5000),
            "$6$xxxxxxxxxxxxxxxx$MgjFZe6f81jv.IGp4u78Sp30q7yi14BHGJVnYpfx6V.6f7V7CvlScL2IhmkRNwOxwGkLHILwis5jE8hpNCBV01"
        );
    }

    #[test]
    fn verifies_a_stored_hash_with_or_without_the_scheme_tag() {
        for text in [HASH.to_string(), format!("{{SHA512-CRYPT}}{HASH}"), format!("operator:{{SHA512-CRYPT}}{HASH}:1000:1000::/:/bin/false")] {
            let c = Credential::parse(&text, "operator").unwrap();
            assert!(c.verify("correct horse"), "{text}");
            assert!(!c.verify("correct horsf"), "{text}");
            assert!(!c.verify(""), "{text}");
        }
    }

    #[test]
    fn a_plain_password_verifies_exactly() {
        let c = Credential::parse("hunter2\n", "operator").unwrap();
        assert!(c.verify("hunter2"));
        assert!(!c.verify("hunter22"));
        assert!(!c.verify("hunter"));
    }

    #[test]
    fn refuses_empty_and_malformed() {
        assert!(Credential::parse("\n", "operator").is_err());
        assert!(Credential::parse("$6$onlysalt", "operator").is_err());
    }

    #[test]
    fn a_world_readable_file_is_refused() {
        let d = testkit::TempDir::new("submission-cred");
        let f = d.join("pw");
        std::fs::write(&f, "x\n").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o644)).unwrap();
        assert!(Credential::load(&f, "operator").unwrap_err().contains("0600"));
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o600)).unwrap();
        assert!(Credential::load(&f, "operator").is_ok());
    }
}
