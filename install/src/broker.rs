//! The installers' call to the one renderer of the MQTT broker's config ([`release::broker`]).

use crate::values::HostValues;
use std::path::Path;

pub use release::broker::{CONF_FILE, UNIT};

/// [`release::broker::place_conf`] with this host's socket and run directory.
pub fn place_conf(templates_dir: &Path, host: &HostValues) -> Result<bool, String> {
    let run = Path::new(&host.run);
    let wrote = release::broker::place_conf(templates_dir, &host.broker_socket, run)?;
    if wrote {
        println!("install: wrote {}", run.join(CONF_FILE).display());
    }
    Ok(wrote)
}
