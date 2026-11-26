//! The MQTT broker's config file, rendered into the run directory where `spira-broker.service`
//! reads it.

use crate::values::{render_file, HostValues};
use std::path::Path;

/// The unit template that reads the config.
pub const UNIT: &str = "spira-broker.service";
/// The config's template name, and its file name in the run directory.
pub const CONF_FILE: &str = "mosquitto.conf";

/// Renders the broker config into the run directory. The file is the harness's, not the
/// operator's to tune, so it is rewritten whenever the rendering differs: a kept file would
/// leave the broker on an old socket path. `Ok(true)` when it was written.
pub fn place_conf(templates_dir: &Path, host: &HostValues) -> Result<bool, String> {
    if host.broker_socket.is_empty() {
        return Err(format!("SPIRA_BROKER_SOCKET is empty — refusing to render {CONF_FILE}"));
    }
    let text = render_file(&templates_dir.join(CONF_FILE), host, None)?;
    let run = Path::new(&host.run);
    let dest = run.join(CONF_FILE);
    if std::fs::read_to_string(&dest).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    // mosquitto creates the socket, never its directory.
    for dir in [Some(run), Path::new(&host.broker_socket).parent()].into_iter().flatten() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    let tmp = run.join(format!("{CONF_FILE}.new"));
    std::fs::write(&tmp, text).and_then(|()| std::fs::rename(&tmp, &dest)).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    println!("install: wrote {}", dest.display());
    Ok(true)
}
