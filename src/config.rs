//! `~/.config/fqshare/config.toml`. Every key is optional.
//!
//! ```toml
//! device_name = "My laptop"      # shown on the phone (default: hostname)
//! download_dir = "/home/me/Downloads"
//! visible_minutes = 5            # how long "Receive" keeps the laptop visible
//! ```

use std::path::PathBuf;

use anyhow::Context;
use serde::Deserialize;

#[derive(Debug, Clone)]
pub struct Config {
    pub device_name: String,
    pub download_dir: PathBuf,
    pub visible_minutes: u64,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    device_name: Option<String>,
    download_dir: Option<PathBuf>,
    visible_minutes: Option<u64>,
}

pub fn load() -> anyhow::Result<Config> {
    let dirs = directories::ProjectDirs::from("", "", "fqshare").context("no home directory")?;
    let path = dirs.config_dir().join("config.toml");
    let file: File = match std::fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", path.display()))?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => File::default(),
        Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
    };

    let device_name = file
        .device_name
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(hostname);
    let download_dir = match file.download_dir {
        Some(p) => p,
        None => directories::UserDirs::new()
            .and_then(|u| u.download_dir().map(PathBuf::from))
            .context("no Downloads folder; set download_dir in the config")?,
    };
    let visible_minutes = file.visible_minutes.unwrap_or(5).clamp(1, 60);

    Ok(Config { device_name, download_dir, visible_minutes })
}

fn hostname() -> String {
    // /etc/hostname may not exist (Fedora); the kernel always knows.
    ["/proc/sys/kernel/hostname", "/etc/hostname"]
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .map(|s| s.trim().to_string())
        .find(|s| !s.is_empty())
        .unwrap_or_else(|| "Linux".into())
}
