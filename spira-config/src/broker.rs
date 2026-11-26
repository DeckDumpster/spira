//! Where the MQTT broker's Unix socket is, for install and every client of the broker.

use std::path::Path;

pub const SOCKET_KEY: &str = "SPIRA_BROKER_SOCKET";

/// [`SOCKET_KEY`] as read through `cfg`; a relative value is a path under `run`. The join
/// happens here, not in the resolver: a resolver step naming a key demands it of every
/// registry it is run against, including one older than the key.
pub fn socket_path(run: &str, cfg: &dyn Fn(&str) -> Result<String, String>) -> Result<String, String> {
    let value = cfg(SOCKET_KEY)?;
    if value.is_empty() {
        return Err(format!("{SOCKET_KEY} is empty: the broker has no socket"));
    }
    Ok(Path::new(run).join(value).to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_id(t6ynf08)]
    #[test]
    fn a_relative_socket_is_under_the_run_dir_an_absolute_one_stands_and_none_is_defaulted() {
        let of = |v: &'static str| socket_path("h/run", &move |_| Ok(v.to_string()));
        assert_eq!(of("nested/b.sock").unwrap(), "h/run/nested/b.sock");
        let absolute: &'static str = format!("{}b.sock", std::path::MAIN_SEPARATOR).leak();
        assert_eq!(of(absolute).unwrap(), absolute);
        assert!(of("").unwrap_err().contains(SOCKET_KEY));
        let undeclared = socket_path("h/run", &|k| Err(format!("{k} is not declared")));
        assert_eq!(undeclared.unwrap_err(), "SPIRA_BROKER_SOCKET is not declared");
    }
}
