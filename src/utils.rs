pub fn time_ago(timestamp: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    let diff = now - timestamp;
    let plural = |n: i64, unit: &str| {
        if n == 1 {
            format!("1 {} ago", unit)
        } else {
            format!("{} {}s ago", n, unit)
        }
    };
    if diff < 60 {
        plural(diff, "second")
    } else if diff < 3600 {
        plural(diff / 60, "minute")
    } else if diff < 86400 {
        plural(diff / 3600, "hour")
    } else if diff < 86400 * 30 {
        plural(diff / 86400, "day")
    } else if diff < 86400 * 365 {
        plural(diff / (86400 * 30), "month")
    } else {
        plural(diff / (86400 * 365), "year")
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
