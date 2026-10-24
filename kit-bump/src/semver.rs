//! The part of Cargo's version-requirement grammar a consumer manifest uses: comma-joined
//! `^ ~ = > >= < <=` comparators, bare versions (caret) and `*`. Pre-release versions are never
//! admitted.

pub type Version = (u64, u64, u64);

pub fn parse_version(s: &str) -> Option<Version> {
    let mut p = s.trim().split('.');
    let v = (p.next()?.parse().ok()?, p.next()?.parse().ok()?, p.next()?.parse().ok()?);
    p.next().is_none().then_some(v)
}

fn part(s: Option<&str>) -> Result<Option<u64>, ()> {
    match s {
        None | Some("*" | "x" | "X") => Ok(None),
        Some(n) => n.parse().map(Some).map_err(|_| ()),
    }
}

fn partial(s: &str) -> Option<(u64, Option<u64>, Option<u64>)> {
    let mut p = s.trim().split('.');
    let major = part(p.next()).ok()??;
    let minor = part(p.next()).ok()?;
    let patch = if minor.is_some() { part(p.next()).ok()? } else { None };
    Some((major, minor, patch))
}

fn comparator(c: &str, v: Version) -> Option<bool> {
    let c = c.trim();
    if c == "*" {
        return Some(true);
    }
    let (op, rest) = ["<=", ">=", "<", ">", "=", "~", "^"].iter().find_map(|o| c.strip_prefix(o).map(|r| (*o, r))).unwrap_or(("^", c));
    let (ma, mi, pa) = partial(rest)?;
    let low = (ma, mi.unwrap_or(0), pa.unwrap_or(0));
    let next_of_given = match (mi, pa) {
        (None, _) => (ma + 1, 0, 0),
        (Some(m), None) => (ma, m + 1, 0),
        (Some(m), Some(p)) => (ma, m, p + 1),
    };
    Some(match op {
        "=" => v >= low && v < next_of_given,
        ">" => v >= next_of_given,
        ">=" => v >= low,
        "<" => v < low,
        "<=" => v < next_of_given,
        "~" => v >= low && v < if mi.is_some() { (ma, mi.unwrap() + 1, 0) } else { (ma + 1, 0, 0) },
        _ => {
            let up = match (ma, mi, pa) {
                (m, _, _) if m > 0 => (m + 1, 0, 0),
                (_, None, _) => (1, 0, 0),
                (_, Some(m), _) if m > 0 => (0, m + 1, 0),
                (_, Some(_), None) => (0, 1, 0),
                (_, Some(_), Some(p)) => (0, 0, p + 1),
            };
            v >= low && v < up
        }
    })
}

/// `None` when `req` is not a requirement this grammar reads.
pub fn admits(req: &str, version: &str) -> Option<bool> {
    let v = parse_version(version)?;
    req.split(',').try_fold(true, |all, c| Some(all & comparator(c, v)?))
}
