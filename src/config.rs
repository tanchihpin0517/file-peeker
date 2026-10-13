//! `config.toml`: the client's settings, as herdr keeps them in its own.
//!
//! The server reads none of this, and none of it is copied to a remote host.
//! A missing file is every default; a file that does not parse is an error
//! rather than a silent fallback, so a typo does not quietly change behavior.

use std::{fs, io};

use serde::Deserialize;

use crate::home::Home;

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct Config {
    pub remote: RemoteConfig,
    pub ui: UiConfig,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct RemoteConfig {
    /// Share one OpenSSH `ControlMaster` connection per machine across setup,
    /// reconnects and `--machine`. Turn it off to use your own SSH
    /// configuration unchanged — for a host whose login needs a `ControlMaster`
    /// of your own making, say.
    pub manage_ssh_config: bool,
}

impl Default for RemoteConfig {
    fn default() -> Self {
        Self {
            manage_ssh_config: true,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, deny_unknown_fields)]
pub(crate) struct UiConfig {
    /// Take the mouse, so the terminal UI can be clicked and scrolled, as
    /// herdr's `ui.mouse_capture`. Turn it off to keep the terminal's own
    /// selection and scrollback, for keyboard-only use.
    pub mouse_capture: bool,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            mouse_capture: true,
        }
    }
}

impl Config {
    /// Reads `home`'s `config.toml`, or every default when there is none.
    ///
    /// # Errors
    ///
    /// Returns a message naming the file when it cannot be read or parsed.
    pub(crate) fn load(home: &Home) -> Result<Self, String> {
        let path = home.config_file();
        match fs::read_to_string(&path) {
            Ok(text) => toml::from_str(&text)
                .map_err(|error| format!("{} is not valid: {error}", path.display())),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(format!("cannot read {}: {error}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn home_with(config: Option<&str>) -> (tempfile::TempDir, Home) {
        let root = tempfile::tempdir().unwrap();
        let home = Home::at(root.path());
        if let Some(config) = config {
            fs::write(home.config_file(), config).unwrap();
        }
        (root, home)
    }

    #[test]
    fn no_file_is_every_default() {
        let (_root, home) = home_with(None);
        let config = Config::load(&home).unwrap();
        assert!(config.remote.manage_ssh_config);
        assert!(config.ui.mouse_capture);
    }

    #[test]
    fn a_setting_overrides_its_default_alone() {
        let (_root, home) = home_with(Some("[remote]\nmanage_ssh_config = false\n"));
        assert!(!Config::load(&home).unwrap().remote.manage_ssh_config);
        let (_root, home) = home_with(Some("# nothing yet\n"));
        assert!(Config::load(&home).unwrap().remote.manage_ssh_config);
        let (_root, home) = home_with(Some("[ui]\nmouse_capture = false\n"));
        let config = Config::load(&home).unwrap();
        assert!(!config.ui.mouse_capture);
        assert!(config.remote.manage_ssh_config);
    }

    #[test]
    fn a_typo_is_an_error_not_a_default() {
        let (_root, home) = home_with(Some("[remote]\nmanage_ssh_cofig = false\n"));
        let error = Config::load(&home).unwrap_err();
        assert!(error.contains("config.toml"), "{error}");
    }
}
