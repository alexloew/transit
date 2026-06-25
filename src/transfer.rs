use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::mpsc::{self, Receiver, Sender, TryRecvError},
    thread,
    time::{Duration, Instant},
};

use crate::remote::{ensure_trailing_slash, join_posix, shell_quote};

#[derive(Debug)]
pub enum TransferEvent {
    Canceled {
        path: PathBuf,
        index: usize,
        total: usize,
    },
    Started {
        path: PathBuf,
        index: usize,
        total: usize,
        bytes_total: Option<u64>,
    },
    Progress {
        path: PathBuf,
        index: usize,
        total: usize,
        bytes_sent: Option<u64>,
        bytes_total: Option<u64>,
        elapsed_secs: u64,
    },
    Finished {
        path: PathBuf,
        index: usize,
        total: usize,
        success: bool,
        message: String,
        bytes_sent: Option<u64>,
        bytes_total: Option<u64>,
        elapsed_secs: u64,
    },
    Complete,
}

#[derive(Debug)]
pub enum TransferCommand {
    Cancel(usize),
}

pub struct TransferHandle {
    pub events: Receiver<TransferEvent>,
    pub commands: Sender<TransferCommand>,
}

pub fn upload(paths: Vec<PathBuf>, host: String, remote_dir: String) -> TransferHandle {
    let (tx, rx) = mpsc::channel();
    let (command_tx, command_rx) = mpsc::channel();

    thread::spawn(move || {
        let destination = scp_destination(&host, &remote_dir);
        let total_paths = paths.len();
        let mut canceled = HashSet::new();

        // Process the upload list as a queue. Only one scp child is active at a time.
        for (offset, path) in paths.into_iter().enumerate() {
            let index = offset + 1;
            drain_cancel_commands(&command_rx, &mut canceled);
            if canceled.contains(&index) {
                let _ = tx.send(TransferEvent::Canceled {
                    path,
                    index,
                    total: total_paths,
                });
                continue;
            }

            let bytes_total = source_size(&path).ok();
            let remote_target = remote_target_path(&remote_dir, &path);
            let _ = tx.send(TransferEvent::Started {
                path: path.clone(),
                index,
                total: total_paths,
                bytes_total,
            });

            let started_at = Instant::now();
            let child = Command::new("scp")
                .arg("-o")
                .arg("BatchMode=yes")
                .arg("-o")
                .arg("ConnectTimeout=5")
                .arg("-o")
                .arg("ConnectionAttempts=1")
                .arg("-r")
                .arg(&path)
                .arg(&destination)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn();

            let event = match child {
                Ok(mut child) => loop {
                    match child.try_wait() {
                        Ok(Some(_)) => {
                            let output = child.wait_with_output();
                            let bytes_sent = remote_size(&host, &remote_target);
                            let elapsed_secs = started_at.elapsed().as_secs();
                            break match output {
                                Ok(output) if output.status.success() => TransferEvent::Finished {
                                    path,
                                    index,
                                    total: total_paths,
                                    success: true,
                                    message: "uploaded".to_string(),
                                    bytes_sent,
                                    bytes_total,
                                    elapsed_secs,
                                },
                                Ok(output) => {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    TransferEvent::Finished {
                                        path,
                                        index,
                                        total: total_paths,
                                        success: false,
                                        message: stderr.trim().to_string(),
                                        bytes_sent,
                                        bytes_total,
                                        elapsed_secs,
                                    }
                                }
                                Err(error) => TransferEvent::Finished {
                                    path,
                                    index,
                                    total: total_paths,
                                    success: false,
                                    message: error.to_string(),
                                    bytes_sent,
                                    bytes_total,
                                    elapsed_secs,
                                },
                            };
                        }
                        Ok(None) => {
                            thread::sleep(Duration::from_secs(1));
                            drain_cancel_commands(&command_rx, &mut canceled);
                            let bytes_sent = remote_size(&host, &remote_target);
                            let _ = tx.send(TransferEvent::Progress {
                                path: path.clone(),
                                index,
                                total: total_paths,
                                bytes_sent,
                                bytes_total,
                                elapsed_secs: started_at.elapsed().as_secs(),
                            });
                        }
                        Err(error) => {
                            break TransferEvent::Finished {
                                path,
                                index,
                                total: total_paths,
                                success: false,
                                message: error.to_string(),
                                bytes_sent: remote_size(&host, &remote_target),
                                bytes_total,
                                elapsed_secs: started_at.elapsed().as_secs(),
                            };
                        }
                    }
                },
                Err(error) => TransferEvent::Finished {
                    path,
                    index,
                    total: total_paths,
                    success: false,
                    message: error.to_string(),
                    bytes_sent: None,
                    bytes_total,
                    elapsed_secs: 0,
                },
            };

            let _ = tx.send(event);
        }

        let _ = tx.send(TransferEvent::Complete);
    });

    TransferHandle {
        events: rx,
        commands: command_tx,
    }
}

fn drain_cancel_commands(rx: &Receiver<TransferCommand>, canceled: &mut HashSet<usize>) {
    loop {
        match rx.try_recv() {
            Ok(TransferCommand::Cancel(index)) => {
                canceled.insert(index);
            }
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
        }
    }
}

fn scp_destination(host: &str, remote_dir: &str) -> String {
    format!("{host}:{}", ensure_trailing_slash(remote_dir))
}

fn remote_target_path(remote_dir: &str, local_path: &Path) -> String {
    local_path
        .file_name()
        .map(|name| join_posix(remote_dir, &name.to_string_lossy()))
        .unwrap_or_else(|| remote_dir.to_string())
}

fn source_size(path: &Path) -> io::Result<u64> {
    let metadata = fs::metadata(path)?;
    if metadata.is_file() {
        return Ok(metadata.len());
    }

    if !metadata.is_dir() {
        return Ok(0);
    }

    let mut size = 0;
    for entry in fs::read_dir(path)? {
        size += source_size(&entry?.path())?;
    }
    Ok(size)
}

fn remote_size(host: &str, remote_path: &str) -> Option<u64> {
    let path = shell_quote(remote_path);
    let script = format!(
        "if [ -d {path} ]; then du -sb {path} 2>/dev/null | awk '{{print $1}}'; elif [ -f {path} ]; then stat -c %s {path} 2>/dev/null; fi"
    );
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
        .ok()?;

    if !output.status.success() {
        return None;
    }

    String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .and_then(|value| value.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{remote_target_path, scp_destination};

    #[test]
    fn builds_scp_destination_without_shell_quotes() {
        assert_eq!(
            scp_destination("user@example", "/remote/media"),
            "user@example:/remote/media/"
        );
        assert_eq!(
            scp_destination("user@example", "/remote/media/TV Shows"),
            "user@example:/remote/media/TV Shows/"
        );
    }

    #[test]
    fn builds_remote_target_path_from_local_file_name() {
        assert_eq!(
            remote_target_path(
                "/remote/media",
                Path::new("/Users/aloewenthal/Downloads/movie.mkv")
            ),
            "/remote/media/movie.mkv"
        );
    }
}
