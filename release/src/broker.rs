//! The MQTT broker's config file, rendered into the run directory where `spira-broker.service`
//! reads it. The one renderer: the installers and activation write the same bytes.

use std::collections::BTreeMap;
use std::path::Path;

/// The unit template that reads the config.
pub const UNIT: &str = "spira-broker.service";
/// The config's template name, and its file name in the run directory.
pub const CONF_FILE: &str = "mosquitto.conf";

/// Renders the broker config into the run directory. The file is the harness's, not the
/// operator's to tune, so it is rewritten whenever the rendering differs: a kept file would
/// leave the broker on an old socket path. `Ok(true)` when it was written.
pub fn place_conf(templates_dir: &Path, socket: &str, run: &Path) -> Result<bool, String> {
    if socket.is_empty() {
        return Err(format!("SPIRA_BROKER_SOCKET is empty — refusing to render {CONF_FILE}"));
    }
    let template = templates_dir.join(CONF_FILE);
    let text = std::fs::read_to_string(&template).map_err(|e| format!("cannot read {}: {e}", template.display()))?;
    let values = BTreeMap::from([("SPIRA_BROKER_SOCKET".to_string(), socket.to_string())]);
    let text = crate::units::render_from_values(CONF_FILE, &text, &values, None, "")?;
    let dest = run.join(CONF_FILE);
    if std::fs::read_to_string(&dest).is_ok_and(|old| old == text) {
        return Ok(false);
    }
    // mosquitto creates the socket, never its directory.
    for dir in [Some(run), Path::new(socket).parent()].into_iter().flatten() {
        std::fs::create_dir_all(dir).map_err(|e| format!("cannot create {}: {e}", dir.display()))?;
    }
    spira_config::write_atomic(&dest, &text).map_err(|e| format!("cannot write {}: {e}", dest.display()))?;
    Ok(true)
}
