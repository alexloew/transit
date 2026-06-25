use std::process::Command;

use anyhow::{Context, Result, anyhow};

#[derive(Clone, Debug)]
pub struct RemoteEntry {
    pub name: String,
    pub path: String,
    pub is_dir: bool,
}

#[derive(Clone, Debug)]
pub struct RemoteListing {
    pub cwd: String,
    pub entries: Vec<RemoteEntry>,
}

pub fn list_dir(host: &str, path: &str) -> Result<RemoteListing> {
    if host.trim().is_empty() {
        return Err(anyhow!("set a remote host first"));
    }

    let script = format!("{} && pwd -P && LC_ALL=C ls -A1p", cd_command(path));
    let output = Command::new("ssh")
        .arg("-o")
        .arg("BatchMode=yes")
        .arg("-o")
        .arg("ConnectTimeout=5")
        .arg("-o")
        .arg("ConnectionAttempts=1")
        .arg(host)
        .arg(script)
        .output()
        .with_context(|| "failed to start ssh")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow!(
            "ssh listing failed: {}",
            stderr.trim().if_empty("unknown error")
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut lines = stdout.lines();
    let cwd = lines.next().unwrap_or(path).to_string();

    let mut entries = lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let is_dir = line.ends_with('/');
            let name = line.trim_end_matches('/').to_string();
            RemoteEntry {
                path: join_posix(&cwd, &name),
                name,
                is_dir,
            }
        })
        .collect::<Vec<_>>();

    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });

    Ok(RemoteListing { cwd, entries })
}

pub fn join_posix(parent: &str, child: &str) -> String {
    let parent = parent.trim_end_matches('/');
    if parent.is_empty() {
        format!("/{child}")
    } else {
        format!("{parent}/{child}")
    }
}

pub fn parent_path(path: &str) -> String {
    let trimmed = path.trim_end_matches('/');
    if trimmed.is_empty() || trimmed == "/" {
        return "/".to_string();
    }

    match trimmed.rsplit_once('/') {
        Some(("", _)) => "/".to_string(),
        Some((parent, _)) => parent.to_string(),
        None => ".".to_string(),
    }
}

pub fn ensure_trailing_slash(path: &str) -> String {
    if path.ends_with('/') {
        path.to_string()
    } else {
        format!("{path}/")
    }
}

pub fn shell_quote(value: &str) -> String {
    if value.is_empty() {
        return "''".to_string();
    }

    format!("'{}'", value.replace('\'', "'\\''"))
}

fn cd_command(path: &str) -> String {
    if path.is_empty() || path == "~" {
        return "cd".to_string();
    }

    if let Some(rest) = path.strip_prefix("~/") {
        return format!("cd \"$HOME\"/{}", shell_quote(rest));
    }

    format!("cd {}", shell_quote(path))
}

trait EmptyFallback {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str;
}

impl EmptyFallback for str {
    fn if_empty<'a>(&'a self, fallback: &'a str) -> &'a str {
        if self.is_empty() { fallback } else { self }
    }
}

#[cfg(test)]
mod tests {
    use super::{cd_command, parent_path, shell_quote};

    #[test]
    fn quotes_remote_shell_paths() {
        assert_eq!(shell_quote("/Volumes/Media"), "'/Volumes/Media'");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\\''s'");
    }

    #[test]
    fn finds_posix_parents() {
        assert_eq!(parent_path("/"), "/");
        assert_eq!(parent_path("/media/movies"), "/media");
        assert_eq!(parent_path("/media/movies/"), "/media");
        assert_eq!(parent_path("relative/path"), "relative");
    }

    #[test]
    fn builds_cd_commands() {
        assert_eq!(cd_command("~"), "cd");
        assert_eq!(
            cd_command("~/Media Library"),
            "cd \"$HOME\"/'Media Library'"
        );
        assert_eq!(cd_command("/Volumes/Media"), "cd '/Volumes/Media'");
    }
}
