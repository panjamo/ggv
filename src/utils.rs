pub fn time_ago(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let diff = now - timestamp;
    if diff < 60 {
        format!("{} seconds ago", diff)
    } else if diff < 3600 {
        format!("{} minutes ago", diff / 60)
    } else if diff < 86400 {
        format!("{} hours ago", diff / 3600)
    } else if diff < 86400 * 30 {
        format!("{} days ago", diff / 86400)
    } else if diff < 86400 * 365 {
        format!("{} months ago", diff / (86400 * 30))
    } else {
        format!("{} years ago", diff / (86400 * 365))
    }
}

pub fn repo_name_from_path(repo_path: &str) -> String {
    let path = std::path::Path::new(repo_path);
    let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    canonical
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("repo")
        .to_string()
}
