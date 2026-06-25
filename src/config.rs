use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Result, anyhow};

pub const DEFAULT_REMOTE_HOST: &str = "";
pub const DEFAULT_REMOTE_PATH: &str = "~";

#[derive(Clone, Debug)]
pub struct AppConfig {
    pub remote_host: String,
    pub remote_path: String,
    pub local_dir: PathBuf,
}

#[derive(Clone, Debug)]
pub struct LoadedConfig {
    pub config: AppConfig,
    pub needs_init: bool,
}

pub fn load(force_init: bool) -> LoadedConfig {
    if let Ok(config) = read_config() {
        return LoadedConfig {
            config,
            needs_init: force_init,
        };
    }

    LoadedConfig {
        config: AppConfig::defaults(),
        needs_init: true,
    }
}

pub fn save(config: &AppConfig) -> Result<()> {
    let path = config_path();
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fs::write(
        path,
        format!(
            "remote_host={}\nremote_path={}\nlocal_dir={}\n",
            config.remote_host,
            config.remote_path,
            config.local_dir.display()
        ),
    )?;
    Ok(())
}

pub fn sanitize_host(value: &str) -> Result<String> {
    let mut host = value.trim();
    if let Some(rest) = host.strip_prefix("ssh://") {
        host = rest.split_once('/').map(|(host, _)| host).unwrap_or(rest);
    }

    if host.is_empty() {
        return Err(anyhow!("remote host cannot be empty"));
    }

    if host.starts_with('-') {
        return Err(anyhow!("remote host cannot start with '-'"));
    }

    if host.chars().any(|ch| ch.is_whitespace() || ch.is_control()) {
        return Err(anyhow!("remote host cannot contain whitespace"));
    }

    let blocked = ['/', '\\', '\'', '"', ';', '|', '&', '`', '$', '<', '>'];
    if host.chars().any(|ch| blocked.contains(&ch)) {
        return Err(anyhow!("remote host contains unsupported characters"));
    }

    Ok(host.to_string())
}

pub fn sanitize_remote_path(value: &str) -> Result<String> {
    let path = value.trim();
    if path.is_empty() {
        return Err(anyhow!("remote path cannot be empty"));
    }
    if path.chars().any(|ch| ch.is_control()) {
        return Err(anyhow!("remote path cannot contain control characters"));
    }
    Ok(path.to_string())
}

pub fn sanitize_local_dir(value: &str) -> Result<PathBuf> {
    let dir = expand_home(value.trim());
    if !dir.is_dir() {
        return Err(anyhow!(
            "source directory does not exist: {}",
            dir.display()
        ));
    }
    Ok(dir)
}

pub fn config_path() -> PathBuf {
    if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
        return PathBuf::from(config_home).join("transit").join("config");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home)
            .join(".config")
            .join("transit")
            .join("config");
    }

    PathBuf::from(".transit-config")
}

impl AppConfig {
    fn defaults() -> Self {
        Self {
            remote_host: DEFAULT_REMOTE_HOST.to_string(),
            remote_path: DEFAULT_REMOTE_PATH.to_string(),
            local_dir: default_local_dir(),
        }
    }
}

fn read_config() -> Result<AppConfig> {
    let content = fs::read_to_string(config_path())?;
    let mut config = AppConfig::defaults();

    for line in content.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        match key.trim() {
            "remote_host" => config.remote_host = sanitize_host(value)?,
            "remote_path" => config.remote_path = sanitize_remote_path(value)?,
            "local_dir" => config.local_dir = sanitize_local_dir(value)?,
            _ => {}
        }
    }

    Ok(config)
}

fn default_local_dir() -> PathBuf {
    let Some(home) = env::var_os("HOME") else {
        return env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    };

    let downloads = PathBuf::from(home).join("Downloads");
    if downloads.is_dir() {
        downloads
    } else {
        env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
    }
}

fn expand_home(value: &str) -> PathBuf {
    if value == "~" {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(value));
    }

    if let Some(rest) = value.strip_prefix("~/") {
        return env::var_os("HOME")
            .map(|home| PathBuf::from(home).join(rest))
            .unwrap_or_else(|| PathBuf::from(value));
    }

    Path::new(value).to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::{sanitize_host, sanitize_remote_path};

    #[test]
    fn sanitizes_ssh_hosts() {
        assert_eq!(sanitize_host(" user@example ").unwrap(), "user@example");
        assert_eq!(
            sanitize_host("ssh://user@example/some/path").unwrap(),
            "user@example"
        );
    }

    #[test]
    fn rejects_unsafe_hosts() {
        assert!(sanitize_host("-oProxyCommand=bad").is_err());
        assert!(sanitize_host("root@host;rm").is_err());
        assert!(sanitize_host("root@host path").is_err());
    }

    #[test]
    fn trims_remote_paths() {
        assert_eq!(
            sanitize_remote_path(" /remote/media ").unwrap(),
            "/remote/media"
        );
    }
}
