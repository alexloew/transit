use std::{
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug)]
pub struct LocalEntry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub file_type: String,
    pub size: Option<u64>,
    pub marked: bool,
}

pub fn read_dir(cwd: &Path, marks: &HashSet<PathBuf>) -> io::Result<Vec<LocalEntry>> {
    let mut entries = Vec::new();

    for entry in fs::read_dir(cwd)? {
        let entry = entry?;
        let path = entry.path();
        let metadata = entry.metadata()?;
        let is_dir = metadata.is_dir();
        let name = entry.file_name().to_string_lossy().into_owned();

        entries.push(LocalEntry {
            name,
            file_type: file_type(&path, is_dir),
            marked: marks.contains(&path),
            size: (!is_dir).then_some(metadata.len()),
            path,
            is_dir,
        });
    }

    entries.sort_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });

    Ok(entries)
}

fn file_type(path: &Path, is_dir: bool) -> String {
    if is_dir {
        return "dir".to_string();
    }

    path.extension()
        .and_then(|extension| extension.to_str())
        .map(|extension| extension.to_lowercase())
        .filter(|extension| !extension.is_empty())
        .unwrap_or_else(|| "file".to_string())
}

pub fn format_size(size: Option<u64>) -> String {
    let Some(size) = size else {
        return String::new();
    };

    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = size as f64;
    let mut unit = 0;

    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }

    if unit == 0 {
        format!("{size} {}", UNITS[unit])
    } else {
        format!("{value:.1} {}", UNITS[unit])
    }
}
