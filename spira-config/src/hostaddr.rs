//! This host's own address, resolved when a consumer runs and never configured: the source
//! address the kernel would use toward a target (a connected UDP socket sends nothing). A DHCP lease that moves
//! the box changes the answer; no config key holds a stale copy of it.

use std::net::UdpSocket;

pub const AUTO: &str = "auto";

/// A documentation-range address nothing owns: the route toward it is the default route, so
/// its source is the host's LAN address.
pub const PROBE: &str = "192.0.2.1";

/// The `src` field of a route description.
pub fn parse_route_src(out: &str) -> Option<String> {
    let mut it = out.split_whitespace();
    while let Some(t) = it.next() {
        if t == "src" {
            return it.next().map(str::to_string);
        }
    }
    None
}

pub fn source_toward_with(target: &str, ip: &dyn Fn(&str) -> Result<String, String>) -> Result<String, String> {
    let out = ip(target)?;
    parse_route_src(&out).ok_or_else(|| format!("no source address toward {target}: {}", out.trim()))
}

fn run_ip(target: &str) -> Result<String, String> {
    let sock = UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("route toward {target}: {e}"))?;
    sock.connect((target, 9)).map_err(|e| format!("route toward {target}: {e}"))?;
    let local = sock.local_addr().map_err(|e| format!("route toward {target}: {e}"))?;
    Ok(format!("{target} src {}", local.ip()))
}

/// The address this host presents toward `target` right now.
pub fn source_toward(target: &str) -> Result<String, String> {
    source_toward_with(target, &run_ip)
}

/// `spec` is [`AUTO`] (or empty): resolve toward `target`. Anything else is a literal
/// address the operator chose, returned as written.
pub fn resolve_with(spec: &str, target: &str, ip: &dyn Fn(&str) -> Result<String, String>) -> Result<String, String> {
    let s = spec.trim();
    if s.is_empty() || s == AUTO {
        source_toward_with(target, ip)
    } else {
        Ok(s.to_string())
    }
}

/// `auto:PORT` becomes `<resolved>:PORT`; `ip:port` and a URL pass through.
pub fn resolve_hostport_with(spec: &str, target: &str, ip: &dyn Fn(&str) -> Result<String, String>) -> Result<String, String> {
    match spec.trim().strip_prefix(AUTO).filter(|r| r.is_empty() || r.starts_with(':')) {
        Some(port) => Ok(format!("{}{port}", source_toward_with(target, ip)?)),
        None => Ok(spec.trim().to_string()),
    }
}

pub fn resolve(spec: &str, target: &str) -> Result<String, String> {
    resolve_with(spec, target, &run_ip)
}

pub fn resolve_hostport(spec: &str) -> Result<String, String> {
    resolve_hostport_with(spec, PROBE, &run_ip)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn route(src: &str) -> impl Fn(&str) -> Result<String, String> + '_ {
        move |t| Ok(format!("{t} via 192.168.1.1 dev ens18 src {src} uid 1000 \\    cache\n"))
    }

    #[test]
    fn src_is_parsed_from_route_output() {
        assert_eq!(parse_route_src("10.0.0.5 dev ens18 src 10.0.0.2 uid 0").as_deref(), Some("10.0.0.2"));
        assert_eq!(parse_route_src("RTNETLINK answers: Network is unreachable"), None);
    }

    #[test]
    fn a_lease_move_between_two_resolutions_needs_no_config() {
        let spec = "auto:9431";
        let a = resolve_hostport_with(spec, PROBE, &route("192.168.1.56")).unwrap();
        let b = resolve_hostport_with(spec, PROBE, &route("192.168.15.174")).unwrap();
        assert_eq!((a.as_str(), b.as_str()), ("192.168.1.56:9431", "192.168.15.174:9431"));
        assert_eq!(resolve_with("auto", "10.1.1.1", &route("192.168.15.174")).unwrap(), "192.168.15.174");
        assert_eq!(resolve_with("", "10.1.1.1", &route("192.168.15.174")).unwrap(), "192.168.15.174");
    }

    #[test]
    fn a_pinned_literal_is_not_resolved_and_so_goes_stale() {
        let asked = Cell::new(false);
        let ip = |_: &str| { asked.set(true); Ok(String::new()) };
        assert_eq!(resolve_with("192.168.1.56", PROBE, &ip).unwrap(), "192.168.1.56");
        assert_eq!(resolve_hostport_with("192.168.1.56:9431", PROBE, &ip).unwrap(), "192.168.1.56:9431");
        assert!(!asked.get());
        let now = resolve_with("auto", PROBE, &route("192.168.15.174")).unwrap();
        assert_ne!(now, "192.168.1.56", "the pinned key no longer names this host");
    }

    #[test]
    fn the_kernel_route_toward_loopback_is_loopback() {
        assert_eq!(source_toward("127.0.0.1").unwrap(), "127.0.0.1");
    }

    #[test]
    fn an_unroutable_host_is_an_error_not_an_empty_address() {
        let ip = |_: &str| Err("unreachable".to_string());
        assert!(resolve_with("auto", PROBE, &ip).is_err());
    }
}
