//! Number and duration formats the line and `env` print.

/// C's `%g`: six significant digits, trailing zeros dropped, exponent form outside 1e-4..1e6.
pub fn g(x: f64) -> String {
    if x == 0.0 {
        return "0".into();
    }
    if !x.is_finite() {
        return if x.is_nan() { "nan".into() } else if x > 0.0 { "inf".into() } else { "-inf".into() };
    }
    let sci = format!("{:.5e}", x);
    let (mant, exp) = sci.split_once('e').unwrap_or((&sci, "0"));
    let exp: i32 = exp.parse().unwrap_or(0);
    if !(-4..6).contains(&exp) {
        let mant = trim_zeros(mant);
        return format!("{mant}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs());
    }
    trim_zeros(&format!("{:.*}", (5 - exp).max(0) as usize, x))
}

fn trim_zeros(s: &str) -> String {
    if s.contains('.') {
        s.trim_end_matches('0').trim_end_matches('.').to_string()
    } else {
        s.to_string()
    }
}

/// Minutes as the coarsest unit that still says something: `48m`, `1h20m`, `3d`.
pub fn dur(minutes: f64) -> String {
    let m = (minutes.round_ties_even() as i64).max(0);
    if m < 60 {
        return format!("{m}m");
    }
    let (h, mm) = (m / 60, m % 60);
    if h < 24 {
        return if mm > 0 { format!("{h}h{mm}m") } else { format!("{h}h") };
    }
    let (d, hh) = (h / 24, h % 24);
    if hh > 0 {
        format!("{d}d{hh}h")
    } else {
        format!("{d}d")
    }
}

/// `HH:MM` in the process's local time zone.
pub fn local_hhmm(epoch: i64) -> String {
    let t = epoch as libc::time_t;
    // SAFETY: localtime_r writes only into `tm`, a zeroed plain struct this frame owns.
    let tm = unsafe {
        let mut tm: libc::tm = std::mem::zeroed();
        libc::localtime_r(&t, &mut tm);
        tm
    };
    format!("{:02}:{:02}", tm.tm_hour, tm.tm_min)
}
