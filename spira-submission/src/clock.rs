//! Civil-calendar arithmetic and the one certificate field the provisioner needs.

pub fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146097 + doe - 719468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719468;
    let era = z.div_euclid(146097);
    let doe = z - era * 146097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (yoe + era * 400 + i64::from(m <= 2), m, d)
}

pub fn iso(secs: i64) -> String {
    let (y, m, d) = civil_from_days(secs.div_euclid(86400));
    let t = secs.rem_euclid(86400);
    format!("{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}Z", t / 3600, t % 3600 / 60, t % 60)
}

pub fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64)
}

fn tlv(b: &[u8]) -> Option<(u8, &[u8], &[u8])> {
    let (&tag, b) = b.split_first()?;
    let (&l0, b) = b.split_first()?;
    let (len, b) = if l0 < 0x80 {
        (l0 as usize, b)
    } else {
        let n = (l0 & 0x7f) as usize;
        if n == 0 || n > 4 || b.len() < n {
            return None;
        }
        (b[..n].iter().fold(0usize, |a, &x| a << 8 | x as usize), &b[n..])
    };
    if b.len() < len {
        return None;
    }
    Some((tag, &b[..len], &b[len..]))
}

fn der_time(tag: u8, v: &[u8]) -> Option<i64> {
    let s = std::str::from_utf8(v).ok()?;
    let (year, rest) = match tag {
        0x17 => {
            let yy: i64 = s.get(..2)?.parse().ok()?;
            (if yy >= 50 { 1900 + yy } else { 2000 + yy }, s.get(2..)?)
        }
        0x18 => (s.get(..4)?.parse().ok()?, s.get(4..)?),
        _ => return None,
    };
    let f = |i: usize| -> Option<i64> { rest.get(i..i + 2)?.parse().ok() };
    Some(days_from_civil(year, f(0)?, f(2)?) * 86400 + f(4)? * 3600 + f(6)? * 60 + f(8)?)
}

/// `notAfter` of a DER certificate as unix seconds.
pub fn not_after(der: &[u8]) -> Option<i64> {
    let (_, cert, _) = tlv(der)?;
    let (_, mut tbs, _) = tlv(cert)?;
    let (tag, _, rest) = tlv(tbs)?;
    if tag == 0xA0 {
        tbs = rest;
    }
    let mut rest = tbs;
    for _ in 0..3 {
        rest = tlv(rest)?.2;
    }
    let (_, validity, _) = tlv(rest)?;
    let (_, _, after_nb) = tlv(validity)?;
    let (tag, v, _) = tlv(after_nb)?;
    der_time(tag, v)
}
