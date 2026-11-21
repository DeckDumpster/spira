//! The symbol-table half of `release verify`: a release binary built with function names
//! stripped is refused (DESIGN.md "verify"). Reads bytes; never runs the binary.

use std::path::Path;

pub const REQUIRED: &[(&str, &[&str])] = &[("bin/spira-lc", &["spira_lc::", "sqlite3_step"])];

const KEEP: &str = "[profile.release] must keep function names (strip = \"debuginfo\")";
const SHT_SYMTAB: u32 = 2;

fn u16_at(b: &[u8], o: usize) -> Result<u16, String> {
    let s = b.get(o..o.checked_add(2).ok_or("offset overflow")?).ok_or("truncated ELF")?;
    Ok(u16::from_le_bytes([s[0], s[1]]))
}
fn u32_at(b: &[u8], o: usize) -> Result<u32, String> {
    let s = b.get(o..o.checked_add(4).ok_or("offset overflow")?).ok_or("truncated ELF")?;
    Ok(u32::from_le_bytes([s[0], s[1], s[2], s[3]]))
}
fn u64_at(b: &[u8], o: usize) -> Result<u64, String> {
    let s = b.get(o..o.checked_add(8).ok_or("offset overflow")?).ok_or("truncated ELF")?;
    Ok(u64::from_le_bytes(s.try_into().unwrap()))
}
fn usize_at(b: &[u8], o: usize) -> Result<usize, String> {
    usize::try_from(u64_at(b, o)?).map_err(|_| "offset overflow".to_string())
}
fn slice(b: &[u8], off: usize, len: usize) -> Result<&[u8], String> {
    b.get(off..off.checked_add(len).ok_or("offset overflow")?).ok_or_else(|| "truncated ELF".to_string())
}

/// The names in every `.symtab` (never `.dynsym`) of an ELF64 little-endian image.
pub fn symtab_names(elf: &[u8]) -> Result<Vec<String>, String> {
    if elf.len() < 64 || &elf[..4] != b"\x7fELF" {
        return Err("not an ELF file".into());
    }
    if elf[4] != 2 || elf[5] != 1 {
        return Err("not ELF64 little-endian".into());
    }
    let shoff = usize_at(elf, 0x28)?;
    let shentsize = u16_at(elf, 0x3a)? as usize;
    let shnum = u16_at(elf, 0x3c)? as usize;
    if shentsize < 64 {
        return Err("section header entries too small".into());
    }
    let sh = |i: usize| -> Result<&[u8], String> { slice(elf, shoff.checked_add(i.checked_mul(shentsize).ok_or("offset overflow")?).ok_or("offset overflow")?, 64) };
    let mut names = Vec::new();
    for i in 0..shnum {
        let h = sh(i)?;
        if u32_at(h, 4)? != SHT_SYMTAB {
            continue;
        }
        let (off, size, link, entsize) = (usize_at(h, 24)?, usize_at(h, 32)?, u32_at(h, 40)? as usize, usize_at(h, 56)?);
        if link >= shnum {
            return Err("symbol table names a missing string table".into());
        }
        let sth = sh(link)?;
        let strtab = slice(elf, usize_at(sth, 24)?, usize_at(sth, 32)?)?;
        let entsize = if entsize == 0 { 24 } else { entsize };
        let table = slice(elf, off, size)?;
        for ent in table.chunks_exact(entsize) {
            let n = u32_at(ent, 0)? as usize;
            let rest = strtab.get(n..).ok_or("symbol name outside its string table")?;
            let end = rest.iter().position(|&c| c == 0).ok_or("unterminated symbol name")?;
            names.push(String::from_utf8_lossy(&rest[..end]).into_owned());
        }
    }
    Ok(names)
}

/// A trailing `::` asks for any symbol whose demangled path lies under that crate (a release
/// inlines `spira_lc::main` itself, leaving its closures); otherwise the exact raw or demangled name.
fn has_symbol(names: &[String], want: &str) -> bool {
    names.iter().any(|n| {
        if n == want {
            return true;
        }
        let Ok(d) = rustc_demangle::try_demangle(n) else { return false };
        let path = format!("{d:#}");
        if want.ends_with("::") {
            path.starts_with(want) || path.starts_with(&format!("<{want}"))
        } else {
            path == want
        }
    })
}

pub fn problems(rel: &Path) -> Vec<String> {
    let mut out = Vec::new();
    for (bin, want) in REQUIRED {
        let bytes = match std::fs::read(rel.join(bin)) {
            Ok(b) => b,
            Err(e) => {
                out.push(format!("{bin}: cannot read: {e}"));
                continue;
            }
        };
        problems_in(bin, &bytes, want, &mut out);
    }
    out
}

fn problems_in(bin: &str, bytes: &[u8], want: &[&str], out: &mut Vec<String>) {
    let names = match symtab_names(bytes) {
        Ok(n) => n,
        Err(e) => {
            out.push(format!("{bin}: {e}"));
            return;
        }
    };
    if names.is_empty() {
        out.push(format!("{bin} carries no symbol table (stripped); {KEEP}"));
        return;
    }
    for w in want {
        if !has_symbol(&names, w) {
            out.push(format!("{bin}: no symbol {w} in its symbol table; {KEEP}"));
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A minimal ELF64 LE image: a `.symtab` (or `.dynsym` when `dynamic`) plus its string table.
    pub(crate) fn elf_image(symbols: &[&str], dynamic: bool) -> Vec<u8> {
        let mut strtab = vec![0u8];
        let mut symtab = vec![0u8; 24];
        for s in symbols {
            let off = strtab.len() as u32;
            strtab.extend_from_slice(s.as_bytes());
            strtab.push(0);
            let mut e = vec![0u8; 24];
            e[..4].copy_from_slice(&off.to_le_bytes());
            symtab.extend(e);
        }
        let symoff = 64u64;
        let stroff = symoff + symtab.len() as u64;
        let shoff = stroff + strtab.len() as u64;
        let mut b = vec![0u8; 64];
        b[..4].copy_from_slice(b"\x7fELF");
        b[4] = 2;
        b[5] = 1;
        b[0x28..0x30].copy_from_slice(&shoff.to_le_bytes());
        b[0x3a..0x3c].copy_from_slice(&64u16.to_le_bytes());
        b[0x3c..0x3e].copy_from_slice(&3u16.to_le_bytes());
        b.extend(&symtab);
        b.extend(&strtab);
        let sec = |ty: u32, off: u64, size: u64, link: u32, ent: u64| {
            let mut h = vec![0u8; 64];
            h[4..8].copy_from_slice(&ty.to_le_bytes());
            h[24..32].copy_from_slice(&off.to_le_bytes());
            h[32..40].copy_from_slice(&size.to_le_bytes());
            h[40..44].copy_from_slice(&link.to_le_bytes());
            h[56..64].copy_from_slice(&ent.to_le_bytes());
            h
        };
        b.extend(sec(0, 0, 0, 0, 0));
        b.extend(sec(if dynamic { 11 } else { SHT_SYMTAB }, symoff, symtab.len() as u64, 2, 24));
        b.extend(sec(3, stroff, strtab.len() as u64, 0, 0));
        b
    }

    // Names as they occur in a release `spira-lc` (v0 mangling; `main` itself is inlined).
    const MAIN: &str = "_RNCNvCsiVYX3EDehpB_8spira_lc4mains0_0B3_";

    fn run(img: &[u8]) -> Vec<String> {
        let mut out = Vec::new();
        problems_in("bin/spira-lc", img, REQUIRED[0].1, &mut out);
        out
    }

    #[test_id(t3qs4um)]
    #[test]
    fn a_symtab_with_both_names_reads_back_and_a_main_closure_is_under_the_crate() {
        let names = symtab_names(&elf_image(&[MAIN, "sqlite3_step"], false)).unwrap();
        assert!(names.contains(&MAIN.to_string()) && names.contains(&"sqlite3_step".to_string()), "{names:?}");
        assert!(has_symbol(&[MAIN.to_string()], "spira_lc::"));
        assert!(!has_symbol(&["sqlite3_step".to_string()], "spira_lc::"));
        assert!(!has_symbol(&["_RNvCs1_5other4main".to_string()], "spira_lc::"));
        assert_eq!(run(&elf_image(&[MAIN, "sqlite3_step"], false)), Vec::<String>::new());
    }

    #[test_id(tr60zbl)]
    #[test]
    fn an_image_with_only_a_dynsym_is_reported_as_carrying_no_symbol_table() {
        let p = run(&elf_image(&["sqlite3_step"], true));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("bin/spira-lc") && p[0].contains("no symbol table"), "{p:?}");
    }

    #[test_id(tloewxz)]
    #[test]
    fn a_symtab_lacking_sqlite3_step_names_the_binary_and_the_missing_symbol() {
        let p = run(&elf_image(&[MAIN], false));
        assert_eq!(p.len(), 1, "{p:?}");
        assert!(p[0].contains("bin/spira-lc") && p[0].contains("sqlite3_step"), "{p:?}");
    }

    #[test_id(twzg6w1)]
    #[test]
    fn truncated_bytes_and_non_elf64_le_headers_are_errors_not_panics() {
        let good = elf_image(&[MAIN], false);
        for n in [0, 10, 63, 70, good.len() - 1] {
            let _ = symtab_names(&good[..n]);
        }
        assert!(symtab_names(&good[..63]).is_err());
        assert!(symtab_names(&good[..good.len() - 100]).is_err());
        let mut b32 = good.clone();
        b32[4] = 1;
        assert!(symtab_names(&b32).is_err());
        let mut be = good;
        be[5] = 2;
        assert!(symtab_names(&be).is_err());
    }

    fn release_strip(text: &str) -> Option<toml::Value> {
        let v: toml::Value = toml::from_str(text).ok()?;
        v.get("profile")?.get("release")?.get("strip").cloned()
    }

    #[test_id(tj1xqo1)]
    #[test]
    fn the_workspace_release_profile_keeps_function_names() {
        assert_eq!(release_strip(include_str!("../../Cargo.toml")), Some(toml::Value::String("debuginfo".into())));
    }

    #[test_id(tc4xw0u)]
    #[test]
    fn the_profile_reader_reads_the_release_table_and_not_another() {
        let v = release_strip("[profile.release]\nstrip = true\n[profile.dev]\nstrip = \"debuginfo\"\n");
        assert_eq!(v, Some(toml::Value::Boolean(true)));
    }
}
