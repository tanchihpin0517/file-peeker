//! `machines.toml`: the SSH machines `fpk machine add` saved.
//!
//! Only what herdr's catalog keeps: an opaque ID, a label, the SSH target and
//! whether it is enabled. Nothing about the host's install or state is stored;
//! that is probed.

use std::{fs, io};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::home::Home;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Machine {
    pub id: String,
    pub label: String,
    /// Anything `ssh` accepts as a destination: `user@host`, or a `Host` alias
    /// from the user's SSH configuration.
    pub ssh: String,
    #[serde(default = "enabled")]
    pub enabled: bool,
}

fn enabled() -> bool {
    true
}

impl Machine {
    pub(crate) fn new(label: &str, ssh: &str) -> Self {
        let id = Uuid::new_v4().simple().to_string();
        Self {
            id: format!("m-{}", &id[..8]),
            label: label.to_owned(),
            ssh: ssh.to_owned(),
            enabled: true,
        }
    }
}

#[derive(Default, Deserialize, Serialize)]
struct Catalog {
    #[serde(default, rename = "machine")]
    machines: Vec<Machine>,
}

/// The saved machines, in the order they were added.
///
/// # Errors
///
/// Returns a message naming the file when it cannot be read or parsed.
pub(crate) fn load(home: &Home) -> Result<Vec<Machine>, String> {
    let path = home.machines_file();
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str::<Catalog>(&text)
            .map(|catalog| catalog.machines)
            .map_err(|error| format!("{} is not valid: {error}", path.display())),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(error) => Err(format!("cannot read {}: {error}", path.display())),
    }
}

/// Replaces the saved machines. Written beside the file and renamed over it,
/// so a crash mid-write leaves the old catalog rather than half of a new one.
///
/// # Errors
///
/// Returns a message naming the file when it cannot be written.
pub(crate) fn save(home: &Home, machines: &[Machine]) -> Result<(), String> {
    let path = home.machines_file();
    let text = toml::to_string_pretty(&Catalog {
        machines: machines.to_vec(),
    })
    .map_err(|error| format!("cannot encode {}: {error}", path.display()))?;
    let partial = path.with_extension("toml.partial");
    home.create()
        .and_then(|()| fs::write(&partial, text))
        .and_then(|()| fs::rename(&partial, &path))
        .map_err(|error| format!("cannot write {}: {error}", path.display()))
}

/// Why a label cannot name a machine, if it cannot.
pub(crate) fn invalid_label(label: &str) -> Option<&'static str> {
    if label.is_empty() {
        Some("a label cannot be empty")
    } else if label == "local" {
        Some("`local` is this machine's own label")
    } else if label.chars().any(|c| c.is_whitespace() || c.is_control()) {
        Some("a label cannot contain spaces")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn saved_machines_load_back_in_order() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::at(root.path().join("home"));
        assert_eq!(load(&home).unwrap(), Vec::new());
        let machines = vec![
            Machine::new("workbox", "me@workbox"),
            Machine::new("nano4", "nano4"),
        ];
        save(&home, &machines).unwrap();
        assert_eq!(load(&home).unwrap(), machines);
        assert!(machines[0].id.starts_with("m-") && machines[0].id.len() == 10);
        assert_ne!(machines[0].id, machines[1].id);
    }

    #[test]
    fn a_hand_written_catalog_defaults_to_enabled() {
        let root = tempfile::tempdir().unwrap();
        let home = Home::at(root.path());
        fs::write(
            home.machines_file(),
            "[[machine]]\nid = \"m-1\"\nlabel = \"box\"\nssh = \"box\"\n",
        )
        .unwrap();
        assert!(load(&home).unwrap()[0].enabled);
    }

    #[test]
    fn labels_are_single_words_and_not_local() {
        assert_eq!(invalid_label("workbox"), None);
        assert!(invalid_label("").is_some());
        assert!(invalid_label("local").is_some());
        assert!(invalid_label("work box").is_some());
    }
}
