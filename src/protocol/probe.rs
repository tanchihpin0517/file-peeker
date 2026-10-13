//! What `fpk probe` says about a host's install, so a client can decide
//! whether to attach, install or update without guessing.
//!
//! The one line a probe prints is JSON. A host with no `fpk` at all cannot run
//! the probe, so the client's probe command prints `{"installed":false}` for it.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub(crate) struct Probe {
    #[serde(default = "installed")]
    pub installed: bool,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub client_protocol: u32,
    /// `release` or `dev`.
    #[serde(default)]
    pub build: String,
    /// For a dev build, the hash of the source it was built from.
    #[serde(default)]
    pub source_hash: Option<String>,
    #[serde(default)]
    pub server_running: bool,
}

fn installed() -> bool {
    true
}

impl Probe {
    /// How a person reads this host's build: `release v0.1.0`, or
    /// `dev, source a1b2c3d4`.
    pub(crate) fn describe(&self) -> String {
        if !self.installed {
            return "nothing installed".to_owned();
        }
        match (self.build.as_str(), &self.source_hash) {
            ("dev", Some(hash)) => format!("dev, source {}", short(hash)),
            ("dev", None) => "dev, source unknown".to_owned(),
            _ => format!("release v{}", self.version),
        }
    }
}

/// The first eight characters of a source hash, which is how it is shown.
pub(crate) fn short(hash: &str) -> &str {
    hash.get(..8).unwrap_or(hash)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_host_without_fpk_says_only_that() {
        let probe: Probe = serde_json::from_str(r#"{"installed":false}"#).unwrap();
        assert!(!probe.installed);
        assert_eq!(probe.describe(), "nothing installed");
    }

    #[test]
    fn builds_describe_themselves() {
        let mut probe = Probe {
            installed: true,
            version: "0.1.0".to_owned(),
            client_protocol: 3,
            build: "release".to_owned(),
            source_hash: None,
            server_running: false,
        };
        assert_eq!(probe.describe(), "release v0.1.0");
        probe.build = "dev".to_owned();
        probe.source_hash = Some("a1b2c3d4e5f6".to_owned());
        assert_eq!(probe.describe(), "dev, source a1b2c3d4");
        let line = serde_json::to_string(&probe).unwrap();
        assert_eq!(serde_json::from_str::<Probe>(&line).unwrap(), probe);
    }
}
