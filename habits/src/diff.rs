use std::path::Path;
use std::process::Command;

pub type Ranges = Vec<(u32, u32)>;

fn git(root: &Path, args: &[&str]) -> Option<String> {
    let o = Command::new("git").arg("-C").arg(root).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

pub fn hits(ranges: &Ranges, start: u32, end: u32) -> bool {
    ranges.iter().any(|&(a, b)| a <= end && start <= b)
}

/// The new-side line ranges `rel` differs from HEAD on; the whole file when it is untracked or the
/// repo has no HEAD, nothing when it is unchanged.
pub fn touched(root: &Path, rel: &str) -> Ranges {
    let whole = vec![(1, u32::MAX)];
    if git(root, &["ls-files", "--error-unmatch", "--", rel]).is_none() {
        return whole;
    }
    match git(root, &["diff", "-U0", "--no-color", "HEAD", "--", rel]) {
        Some(d) => parse_hunks(&d),
        None => whole,
    }
}

pub fn parse_hunks(diff: &str) -> Ranges {
    diff.lines()
        .filter_map(|l| l.strip_prefix("@@ "))
        .filter_map(|l| {
            let plus = l.split_whitespace().find(|t| t.starts_with('+'))?;
            let (start, len) = match plus[1..].split_once(',') {
                Some((s, n)) => (s.parse().ok()?, n.parse().ok()?),
                None => (plus[1..].parse().ok()?, 1),
            };
            Some(if len == 0 { (start, start + 1) } else { (start, start + len - 1) })
        })
        .collect()
}

pub fn head_source(root: &Path, rel: &str) -> Option<String> {
    git(root, &["show", &format!("HEAD:{rel}")])
}

/// Files changed against HEAD plus untracked ones, relative to the repo root.
pub fn changed_files(root: &Path) -> Vec<String> {
    let mut v: Vec<String> = Vec::new();
    for args in [&["diff", "--name-only", "HEAD"][..], &["ls-files", "--others", "--exclude-standard"][..]] {
        v.extend(git(root, args).unwrap_or_default().lines().map(str::to_string));
    }
    v.sort();
    v.dedup();
    v
}

#[cfg(test)]
mod tests {
    use super::parse_hunks;

    #[test]
    fn hunks() {
        assert_eq!(parse_hunks("@@ -1,0 +2,27 @@ fn x
@@ -9 +40 @@
@@ -5,2 +7,0 @@
"), vec![(2, 28), (40, 40), (7, 8)]);
    }
}
