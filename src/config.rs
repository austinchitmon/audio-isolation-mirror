use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct AppConfig {
    pub input_device_name: Option<String>,
    pub output_device_name: Option<String>,
    pub mode_code: Option<u8>,
    #[serde(default)]
    pub muted: bool,
    #[serde(default)]
    pub dev_console: bool,
}

fn config_path() -> Option<PathBuf> {
    dirs_next_config_dir().map(|dir| dir.join("audio-isolation-mirror").join("config.json"))
}

// Minimal stand-in for the `dirs` crate so we don't pull in another dependency
// just for one path lookup.
fn dirs_next_config_dir() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(PathBuf::from)
}

impl AppConfig {
    pub fn load() -> Self {
        config_path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(path) = config_path() else { return };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        if let Ok(json) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, json);
        }
    }
}
