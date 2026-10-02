//! The gate runs in its own transient systemd scope sized by `SPIRA_GATE_CPU_QUOTA`, so the
//! trial's CPU is bounded by the kernel and every child inherits the bound.

use std::path::Path;

pub const QUOTA_ENV: &str = "SPIRA_GATE_CPU_QUOTA";

/// `"400"` and `"400%"` both mean four cores; anything else is refused, never guessed.
pub fn quota_percent(raw: &str) -> Result<u64, String> {
    let digits = raw.strip_suffix('%').unwrap_or(raw);
    match digits.parse::<u64>() {
        Ok(n) if n > 0 && digits.bytes().all(|b| b.is_ascii_digit()) => Ok(n),
        _ => Err(format!("{QUOTA_ENV}={raw:?} is not a positive percent")),
    }
}

pub fn scope_argv(quota: &str, exe: &Path, args: &[String]) -> Result<Vec<String>, String> {
    let pct = quota_percent(quota)?;
    let mut v: Vec<String> = [
        "systemd-run",
        "--user",
        "--scope",
        "--collect",
        "--quiet",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    v.push(format!("--property=CPUQuota={pct}%"));
    v.push("--".into());
    v.push(exe.to_string_lossy().into_owned());
    v.extend(args.iter().cloned());
    Ok(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_reads_with_or_without_percent() {
        assert_eq!(quota_percent("400"), Ok(400));
        assert_eq!(quota_percent("250%"), Ok(250));
    }

    #[test]
    fn quota_refuses_what_is_not_a_positive_percent() {
        for bad in ["", "0", "%", "-5", "4.5", "x%", "10%%", "+3"] {
            assert!(quota_percent(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn argv_wraps_self_in_a_scope_carrying_the_args() {
        let a = scope_argv("300", Path::new("/r/bin/gate"), &["--home".into(), "/h".into(), "br".into()]).unwrap();
        assert_eq!(&a[..2], ["systemd-run", "--user"]);
        assert!(a.contains(&"--scope".to_string()));
        assert!(a.contains(&"--property=CPUQuota=300%".to_string()));
        let dash = a.iter().position(|x| x == "--").unwrap();
        assert_eq!(&a[dash + 1..], ["/r/bin/gate", "--home", "/h", "br"]);
        assert!(scope_argv("nope", Path::new("/g"), &[]).is_err());
    }
}
