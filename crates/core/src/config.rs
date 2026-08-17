//! Persisted configuration.
//!
//! One JSON file holds everything that must survive a restart: the screen
//! arrangement, hotkeys, and the fingerprints of paired machines. It is written
//! atomically (temp file + rename) because a truncated config would drop the
//! pairing state and force the user to re-approve every machine.

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hotkey::{Action, Hotkey, HotkeyTable};
use crate::keymap::ModifierMap;
use crate::layout::{Layout, MachineId};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("io error on {path}: {source}")]
    Io {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("could not parse {path}: {source}")]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("no home or config directory could be determined")]
    NoConfigDir,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    /// Owns the physical keyboard and mouse.
    Host,
    /// Receives input from a host.
    Client,
}

/// A machine this one has paired with, identified by certificate fingerprint.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PairedPeer {
    pub machine: MachineId,
    pub name: String,
    /// Lowercase hex SHA-256 of the peer's DER certificate. This is the whole
    /// of the trust decision: TLS verifies the peer holds the matching key, and
    /// this says we have met that key before and approved it.
    pub fingerprint: String,
    /// Last address it was seen at. A hint for reconnecting before discovery
    /// finds it again, never a substitute for the fingerprint check.
    pub last_address: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Options {
    pub sync_clipboard: bool,
    pub sync_clipboard_images: bool,
    /// Lock this machine's screen when the cursor leaves it.
    pub lock_screen_on_leave: bool,
    pub enable_file_transfer: bool,
    /// Where received files land. `None` means the OS downloads directory.
    pub file_transfer_dir: Option<PathBuf>,
    /// Start with the cursor locked to the host.
    pub cursor_lock_on_start: bool,
    /// Pixels of overshoot required at a screen edge before the cursor crosses.
    /// Zero is seamless but makes it easy to overshoot onto another machine
    /// when aiming at a scrollbar or a Dock icon.
    pub edge_resistance: u32,
    /// Milliseconds between heartbeats. Also the reconnect poll interval.
    pub heartbeat_ms: u32,
    /// Per-peer modifier overrides, keyed by machine. Absent means use the
    /// platform default from `ModifierMap::between`.
    pub modifier_overrides: Vec<(MachineId, ModifierMap)>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            sync_clipboard: true,
            sync_clipboard_images: true,
            lock_screen_on_leave: false,
            enable_file_transfer: true,
            file_transfer_dir: None,
            cursor_lock_on_start: false,
            edge_resistance: 0,
            heartbeat_ms: 2_000,
            modifier_overrides: Vec::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// Schema version of this file, so a future release can migrate it.
    pub version: u32,
    /// Name shown to other machines. Defaults to the system hostname.
    pub name: String,
    pub role: Role,
    /// Address to bind (host) or connect to (client). `None` on a client means
    /// "use discovery".
    pub address: Option<String>,
    pub port: u16,
    pub layout: Layout,
    pub hotkeys: HotkeyTable,
    pub peers: Vec<PairedPeer>,
    pub options: Options,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            version: 1,
            name: default_machine_name(),
            role: Role::Client,
            address: None,
            port: tether_proto::DEFAULT_PORT,
            layout: Layout::new(),
            hotkeys: default_hotkeys(),
            peers: Vec::new(),
            options: Options::default(),
        }
    }
}

impl Config {
    /// Load from the default location, or return defaults if it does not exist.
    /// A *malformed* file is an error rather than a silent reset — quietly
    /// discarding a user's arrangement is worse than refusing to start.
    pub fn load_or_default() -> Result<Self, ConfigError> {
        let path = Self::default_path()?;
        match Self::load(&path) {
            Err(ConfigError::Io { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
                Ok(Self::default())
            }
            other => other,
        }
    }

    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        })?;
        serde_json::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })
    }

    pub fn save_default(&self) -> Result<PathBuf, ConfigError> {
        let path = Self::default_path()?;
        self.save(&path)?;
        Ok(path)
    }

    /// Atomic write: serialise to `<path>.tmp`, then rename over the target.
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let io_err = |source| ConfigError::Io {
            path: path.to_path_buf(),
            source,
        };

        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io_err)?;
        }
        let json = serde_json::to_string_pretty(self).map_err(|source| ConfigError::Parse {
            path: path.to_path_buf(),
            source,
        })?;

        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, json).map_err(io_err)?;
        std::fs::rename(&tmp, path).map_err(io_err)?;
        Ok(())
    }

    pub fn default_path() -> Result<PathBuf, ConfigError> {
        Ok(config_dir()?.join("config.json"))
    }

    pub fn peer(&self, machine: MachineId) -> Option<&PairedPeer> {
        self.peers.iter().find(|p| p.machine == machine)
    }

    pub fn peer_by_fingerprint(&self, fingerprint: &str) -> Option<&PairedPeer> {
        self.peers
            .iter()
            .find(|p| p.fingerprint.eq_ignore_ascii_case(fingerprint))
    }

    pub fn add_peer(&mut self, peer: PairedPeer) {
        self.peers.retain(|p| p.machine != peer.machine);
        self.peers.push(peer);
    }

    pub fn remove_peer(&mut self, machine: MachineId) {
        self.peers.retain(|p| p.machine != machine);
        self.layout.remove(machine);
        self.hotkeys
            .bindings
            .retain(|b| b.action != Action::SwitchTo { machine });
    }

    /// The modifier map to use for a peer: its override, or the platform
    /// default for the two platforms involved.
    pub fn modifier_map_for(
        &self,
        machine: MachineId,
        from: tether_proto::Platform,
        to: tether_proto::Platform,
    ) -> ModifierMap {
        self.options
            .modifier_overrides
            .iter()
            .find(|(m, _)| *m == machine)
            .map(|(_, map)| *map)
            .unwrap_or_else(|| ModifierMap::between(from, to))
    }
}

/// Bindings present on a fresh install.
///
/// Ctrl+Alt is used as the prefix because it is the one chord that is not
/// spoken for by any of the three platforms' window managers.
fn default_hotkeys() -> HotkeyTable {
    let mut table = HotkeyTable::new();
    if let Ok(hk) = "ctrl+alt+l".parse::<Hotkey>() {
        table.bind(hk, Action::ToggleLock);
    }
    if let Ok(hk) = "ctrl+alt+home".parse::<Hotkey>() {
        table.bind(hk, Action::RecallCursor);
    }
    // Per-machine switch hotkeys (ctrl+alt+1..9) are added by the setup wizard
    // as machines are paired, since they need a MachineId to point at.
    table
}

/// Hostname, falling back to a fixed string. Used as the default display name.
pub fn default_machine_name() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "tether".to_string())
}

/// Per-user config directory, following each platform's convention.
pub fn config_dir() -> Result<PathBuf, ConfigError> {
    #[cfg(target_os = "macos")]
    {
        let home = std::env::var_os("HOME").ok_or(ConfigError::NoConfigDir)?;
        Ok(PathBuf::from(home).join("Library/Application Support/Tether"))
    }
    #[cfg(target_os = "windows")]
    {
        let base = std::env::var_os("APPDATA").ok_or(ConfigError::NoConfigDir)?;
        Ok(PathBuf::from(base).join("Tether"))
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(xdg).join("tether"));
        }
        let home = std::env::var_os("HOME").ok_or(ConfigError::NoConfigDir)?;
        Ok(PathBuf::from(home).join(".config/tether"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_round_trip_through_json() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        let back: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(config, back);
    }

    #[test]
    fn an_older_partial_file_fills_in_defaults() {
        // Every field is `#[serde(default)]`, so a config written by an earlier
        // version that lacked `options` still loads.
        let back: Config = serde_json::from_str(r#"{"name":"desk","role":"host"}"#).unwrap();
        assert_eq!(back.name, "desk");
        assert_eq!(back.role, Role::Host);
        assert_eq!(back.port, tether_proto::DEFAULT_PORT);
        assert!(back.options.sync_clipboard);
    }

    #[test]
    fn removing_a_peer_also_clears_its_layout_and_hotkeys() {
        let mut config = Config::default();
        let defaults = config.hotkeys.bindings.len();
        let id = MachineId(7);
        config.add_peer(PairedPeer {
            machine: id,
            name: "laptop".into(),
            fingerprint: "ab".into(),
            last_address: None,
        });
        config
            .hotkeys
            .bind("ctrl+alt+2".parse().unwrap(), Action::SwitchTo { machine: id });

        config.remove_peer(id);

        assert!(config.peers.is_empty());
        assert!(!config.layout.contains(id));
        // The machine's switch hotkey is gone; the global defaults survive.
        assert_eq!(config.hotkeys.bindings.len(), defaults);
        assert!(!config
            .hotkeys
            .bindings
            .iter()
            .any(|b| b.action == Action::SwitchTo { machine: id }));
    }

    #[test]
    fn fingerprint_lookup_is_case_insensitive() {
        let mut config = Config::default();
        config.add_peer(PairedPeer {
            machine: MachineId(1),
            name: "n".into(),
            fingerprint: "AbCd".into(),
            last_address: None,
        });
        assert!(config.peer_by_fingerprint("abcd").is_some());
    }

    #[test]
    fn saving_and_loading_a_config_preserves_it() {
        let dir = std::env::temp_dir().join(format!("tether-test-{}", std::process::id()));
        let path = dir.join("config.json");
        let config = Config {
            name: "roundtrip".into(),
            ..Config::default()
        };
        config.save(&path).unwrap();

        assert_eq!(Config::load(&path).unwrap(), config);
        // The temp file must not survive a successful write.
        assert!(!path.with_extension("json.tmp").exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
