use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::process::Stdio;

const DEFAULT_BROWSER_PROMPT: &str = "Summarize the changes.
        At the beginning, I would like a paragraph that is two sentences long, where everything is summarized very briefly.
        After that, you can calmly write a bit more.
        The summary should be nicely characterized by headings.
        Structure the whole thing.";

const DEFAULT_DIFF_PROMPT: &str = "short summarize the git diff output, focus on the most important changes and their implications.
        The summary should be concise and structured with headings if needed.";

const DEFAULT_LOG_PROMPT: &str = "Summarize the commit history. Focus on what changed and why, based on commit messages and file names only.
        Be concise and structured with headings if needed.";

/// Git log format used as metadata context when feeding diffs to the AI.
const GIT_LOG_METADATA_FORMAT: &str =
    "--pretty=format:commit %H%nRefs: %D%nAuthor: %an <%ae>%nDate: %ci%nSubject: %s%n";

/// Minimal HTML page that closes itself — sent after fire-and-forget browser actions.
const HTML_CLOSE_WINDOW: &str = "<html><body><script>window.close();</script></body></html>";

pub fn base_url(port: u16) -> String {
    format!("http://[::1]:{}", port)
}

/// Binds to the given port (0 = OS-assigned) and spawns the server thread.
/// Returns the join handle and the actual bound port.
pub fn start(
    port: u16,
    repo_path: String,
    svg_path: String,
    gia_browser: bool,
    prompt: Option<String>,
) -> anyhow::Result<(std::thread::JoinHandle<()>, u16)> {
    let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port);
    let listener = TcpListener::bind(addr)?;
    let actual_port = listener.local_addr()?.port();
    eprintln!(
        "Diff server listening on http://[::1]:{} (Ctrl+C to stop)",
        actual_port
    );
    let handle = std::thread::spawn(move || {
        run_server(listener, &repo_path, &svg_path, gia_browser, prompt)
    });
    Ok((handle, actual_port))
}

fn run_server(
    listener: TcpListener,
    repo_path: &str,
    svg_path: &str,
    gia_browser: bool,
    prompt: Option<String>,
) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let repo_clone = repo_path.to_string();
                let svg_clone = svg_path.to_string();
                let prompt_clone = prompt.clone();
                std::thread::spawn(move || {
                    handle_connection(stream, &repo_clone, &svg_clone, gia_browser, prompt_clone)
                });
            }
            Err(e) => eprintln!("Connection error: {}", e),
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    repo_path: &str,
    svg_path: &str,
    gia_browser: bool,
    prompt: Option<String>,
) {
    let reader = BufReader::new(match stream.try_clone() {
        Ok(s) => s,
        Err(_) => return,
    });
    let request_line = match reader.lines().next() {
        Some(Ok(line)) => line,
        _ => return,
    };

    let parts: Vec<&str> = request_line.splitn(3, ' ').collect();
    if parts.len() < 2 || parts[0] != "GET" {
        return;
    }

    let path_and_query = parts[1];
    let (path, query) = match path_and_query.find('?') {
        Some(idx) => (&path_and_query[..idx], &path_and_query[idx + 1..]),
        None => (path_and_query, ""),
    };

    match path {
        "/view" => {
            serve_svg(&mut stream, svg_path);
        }
        "/checkout" => {
            let params = parse_query(query);
            let sha = match params.get("sha") {
                Some(s) if is_valid_sha(s) => s.clone(),
                _ => {
                    send_response(&mut stream, 400, "text/plain", "Invalid or missing 'sha'");
                    return;
                }
            };
            run_git_checkout(repo_path, &sha);
            send_response(&mut stream, 200, "text/plain", "OK");
        }
        "/delete-branch" => {
            let params = parse_query(query);
            let name = match params.get("name") {
                Some(n) if is_valid_ref_name(n) => n.clone(),
                _ => {
                    send_response(&mut stream, 400, "text/plain", "Invalid or missing 'name'");
                    return;
                }
            };
            let scope = params.get("scope").map(|s| s.as_str()).unwrap_or("local");
            run_branch_delete(repo_path, &name, scope);
            send_response(&mut stream, 200, "text/plain", "OK");
        }
        "/diff" => {
            let params = parse_query(query);
            let sha1 = match params.get("from") {
                Some(s) if is_valid_sha(s) => s.clone(),
                _ => {
                    send_response(
                        &mut stream,
                        400,
                        "text/plain",
                        "Invalid or missing 'from' parameter",
                    );
                    return;
                }
            };
            let sha2 = match params.get("to") {
                Some(s) if is_valid_sha(s) => s.clone(),
                _ => {
                    send_response(
                        &mut stream,
                        400,
                        "text/plain",
                        "Invalid or missing 'to' parameter",
                    );
                    return;
                }
            };

            let force_ai = params.get("ai").map(|v| v == "1").unwrap_or(false);

            if !force_ai {
                if has_git_diff(repo_path, &sha1, &sha2) {
                    run_git_difftool(repo_path, &sha1, &sha2);
                } else {
                    send_response(
                        &mut stream,
                        200,
                        "text/html; charset=utf-8",
                        &build_no_diff_html(&sha1, &sha2),
                    );
                    return;
                }
            }

            if !force_ai {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    HTML_CLOSE_WINDOW,
                );
                return;
            }

            let effective_prompt = prompt.as_deref();
            if gia_browser {
                run_gia_browser(repo_path, &sha1, &sha2, effective_prompt);
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    HTML_CLOSE_WINDOW,
                );
            } else {
                let summary = run_gia_diff(repo_path, &sha1, &sha2, effective_prompt);
                let html = build_html(
                    &sha1[..sha1.len().min(7)],
                    &sha2[..sha2.len().min(7)],
                    &summary,
                );
                send_response(&mut stream, 200, "text/html; charset=utf-8", &html);
            }
        }
        "/log-summary" => {
            let params = parse_query(query);
            let sha1 = match params.get("from") {
                Some(s) if is_valid_sha(s) => s.clone(),
                _ => {
                    send_response(&mut stream, 400, "text/plain", "Invalid or missing 'from'");
                    return;
                }
            };
            let sha2 = match params.get("to") {
                Some(s) if is_valid_sha(s) => s.clone(),
                _ => {
                    send_response(&mut stream, 400, "text/plain", "Invalid or missing 'to'");
                    return;
                }
            };
            let effective_prompt = prompt.as_deref();
            if gia_browser {
                run_gia_log_browser(repo_path, &sha1, &sha2, effective_prompt);
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    HTML_CLOSE_WINDOW,
                );
            } else {
                let summary = run_gia_log(repo_path, &sha1, &sha2, effective_prompt);
                let html = build_html(
                    &sha1[..sha1.len().min(7)],
                    &sha2[..sha2.len().min(7)],
                    &summary,
                );
                send_response(&mut stream, 200, "text/html; charset=utf-8", &html);
            }
        }
        "/log" => {
            let params = parse_query(query);
            let sha1 = match params.get("from") {
                Some(s) if is_valid_sha(s) => s.clone(),
                _ => {
                    send_response(&mut stream, 400, "text/plain", "Invalid or missing 'from'");
                    return;
                }
            };
            let sha2 = match params.get("to") {
                Some(s) if is_valid_sha(s) => s.clone(),
                _ => {
                    send_response(&mut stream, 400, "text/plain", "Invalid or missing 'to'");
                    return;
                }
            };
            let html = serve_git_log(repo_path, &sha1, &sha2);
            send_response(&mut stream, 200, "text/html; charset=utf-8", &html);
        }
        _ => {
            send_response(&mut stream, 404, "text/plain", "Not Found");
        }
    }
}

fn serve_svg(stream: &mut TcpStream, svg_path: &str) {
    let svg_content = match std::fs::read_to_string(svg_path) {
        Ok(c) => c,
        Err(_) => {
            send_response(stream, 404, "text/plain", "SVG not yet available");
            return;
        }
    };
    // Strip XML declaration / DOCTYPE — only keep from <svg onward
    let svg_body = if let Some(pos) = svg_content.find("<svg") {
        &svg_content[pos..]
    } else {
        svg_content.as_str()
    };
    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>GGV</title>
<style>
  body {{ margin: 0; background: #1a1f2e; overflow: auto; }}
  svg {{ display: block; }}
</style>
</head>
<body>{}</body>
</html>"#,
        svg_body
    );
    send_response(stream, 200, "text/html; charset=utf-8", &html);
}

fn run_git_checkout(repo_path: &str, sha: &str) {
    // Find a local branch pointing at this SHA and check it out.
    // Fall back to checking out the SHA directly (detached HEAD).
    let branch_out = std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "branch",
            "--points-at",
            sha,
            "--format=%(refname:short)",
        ])
        .output();

    if let Ok(out) = branch_out {
        if let Ok(text) = std::str::from_utf8(&out.stdout) {
            if let Some(branch) = text.lines().find(|l| !l.is_empty()) {
                let _ = std::process::Command::new("git")
                    .args(["-C", repo_path, "checkout", branch])
                    .status();
                return;
            }
        }
    }

    let _ = std::process::Command::new("git")
        .args(["-C", repo_path, "checkout", sha])
        .status();
}

fn run_branch_delete(repo_path: &str, name: &str, scope: &str) {
    match scope {
        "local" => {
            let _ = std::process::Command::new("git")
                .args(["-C", repo_path, "branch", "-D", name])
                .status();
        }
        "remote" => {
            // name is "remote/branch" — split on the first '/'
            let (remote, branch) = if let Some(idx) = name.find('/') {
                (&name[..idx], &name[idx + 1..])
            } else {
                ("origin", name)
            };
            let _ = std::process::Command::new("git")
                .args(["-C", repo_path, "push", remote, "--delete", branch])
                .status();
        }
        _ => {}
    }
}

fn is_valid_ref_name(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 256
        && s.chars()
            .all(|c| c.is_alphanumeric() || matches!(c, '-' | '_' | '.' | '/' | '@' | '~'))
}

fn is_valid_sha(s: &str) -> bool {
    s.len() >= 7 && s.len() <= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn percent_decode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let (Some(h), Some(l)) = (
                (bytes[i + 1] as char).to_digit(16),
                (bytes[i + 2] as char).to_digit(16),
            ) {
                out.push((h * 16 + l) as u8 as char);
                i += 3;
                continue;
            }
        }
        if bytes[i] == b'+' {
            out.push(' ');
        } else {
            out.push(bytes[i] as char);
        }
        i += 1;
    }
    out
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(k.to_string(), percent_decode(v));
        }
    }
    map
}

fn has_git_diff(repo_path: &str, sha1: &str, sha2: &str) -> bool {
    std::process::Command::new("git")
        .args(["-C", repo_path, "diff", "--quiet", sha1, sha2])
        .status()
        .map(|s| !s.success())
        .unwrap_or(true)
}

fn build_no_diff_html(sha1: &str, sha2: &str) -> String {
    let s1 = &sha1[..sha1.len().min(7)];
    let s2 = &sha2[..sha2.len().min(7)];
    format!(
        r#"<html><head><meta charset="utf-8"><style>
body{{font-family:Arial,sans-serif;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:#f5f5f5}}
.box{{background:#fff;border:1px solid #ccc;border-radius:6px;padding:24px 32px;text-align:center;box-shadow:0 2px 8px rgba(0,0,0,.12)}}
h3{{margin:0 0 8px}}p{{margin:0 0 16px;color:#555}}button{{padding:6px 20px;cursor:pointer}}
</style></head><body><div class="box">
<h3>No Differences Found</h3>
<p>{s1} &rarr; {s2} are identical.</p>
<button onclick="window.close()">Close</button>
</div></body></html>"#
    )
}

fn run_git_difftool(repo_path: &str, sha1: &str, sha2: &str) {
    let _ = std::process::Command::new("git")
        .args(["-C", repo_path, "difftool", "-d", sha1, sha2])
        .spawn();
}

fn run_gia_browser(repo_path: &str, sha1: &str, sha2: &str, prompt: Option<&str>) {
    let base = match resolve_diff_base(repo_path, sha1, sha2) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error resolving diff base: {e}");
            return;
        }
    };

    let diff = match std::process::Command::new("git")
        .args(["-C", repo_path, "diff", &base, sha2])
        .output()
    {
        Ok(out) => out.stdout,
        Err(e) => {
            eprintln!("git diff error: {e}");
            return;
        }
    };

    let log_range = format!("{}..{}", base, sha2);
    let log_out = std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            GIT_LOG_METADATA_FORMAT,
            "--name-status",
            &log_range,
        ])
        .output()
        .ok();
    let metadata = log_out.map(|o| o.stdout).unwrap_or_default();

    let meta_path = std::env::temp_dir().join("ggv_meta_browser.txt");
    let has_meta = !metadata.is_empty() && std::fs::write(&meta_path, &metadata).is_ok();

    let effective_prompt = prompt.unwrap_or(DEFAULT_BROWSER_PROMPT);
    let mut gia_args: Vec<String> = vec!["-b".to_string(), effective_prompt.to_string()];
    if has_meta {
        gia_args.push("-f".to_string());
        gia_args.push(meta_path.to_string_lossy().into_owned());
    }

    let mut gia = match std::process::Command::new("gia")
        .args(&gia_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("gia error: {e}");
            return;
        }
    };

    if let Some(mut stdin) = gia.stdin.take() {
        let _ = stdin.write_all(&diff);
    }
    // fire-and-forget: gia opens its own window
    let _ = gia.wait();

    if has_meta {
        let _ = std::fs::remove_file(&meta_path);
    }
}

fn resolve_diff_base(repo_path: &str, sha1: &str, sha2: &str) -> Result<String, String> {
    // Check if sha1 is a direct ancestor of sha2
    let ancestor_status = std::process::Command::new("git")
        .args(["-C", repo_path, "merge-base", "--is-ancestor", sha1, sha2])
        .status()
        .map_err(|e| format!("git merge-base --is-ancestor failed to start: {e}"))?;
    if ancestor_status.success() {
        return Ok(sha1.to_string());
    }

    // Fall back to merge-base
    let out = std::process::Command::new("git")
        .args(["-C", repo_path, "merge-base", sha1, sha2])
        .output()
        .map_err(|e| format!("git merge-base failed to start: {e}"))?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        return Err(format!(
            "git merge-base exited with {}: {}",
            out.status, stderr
        ));
    }
    let mb = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if mb.is_empty() {
        return Err("git merge-base returned empty output".to_string());
    }
    Ok(mb)
}

fn run_gia_diff(repo_path: &str, sha1: &str, sha2: &str, prompt: Option<&str>) -> String {
    let base = match resolve_diff_base(repo_path, sha1, sha2) {
        Ok(b) => b,
        Err(e) => return format!("Error resolving diff base: {e}"),
    };

    // Snapshot diff: diff(base, sha2)
    let diff_out = match std::process::Command::new("git")
        .args(["-C", repo_path, "diff", &base, sha2])
        .output()
    {
        Ok(out) => out,
        Err(e) => return format!("Error running git diff: {e}"),
    };
    if !diff_out.status.success() {
        let stderr = String::from_utf8_lossy(&diff_out.stderr).trim().to_string();
        return format!("git diff exited with {}: {}", diff_out.status, stderr);
    }
    let diff = diff_out.stdout;

    if diff.is_empty() {
        return "No differences found between these commits.".to_string();
    }

    // Commit metadata: log(base..sha2) with branch/tag decorations
    let log_range = format!("{}..{}", base, sha2);
    let log_out = match std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            GIT_LOG_METADATA_FORMAT,
            "--name-status",
            &log_range,
        ])
        .output()
    {
        Ok(out) => out,
        Err(e) => return format!("Error running git log: {e}"),
    };
    if !log_out.status.success() {
        let stderr = String::from_utf8_lossy(&log_out.stderr).trim().to_string();
        return format!("git log exited with {}: {}", log_out.status, stderr);
    }
    let metadata = log_out.stdout;

    // Write metadata to a temp file for gia -f
    let meta_path = std::env::temp_dir().join("ggv_meta.txt");
    let has_meta = !metadata.is_empty() && std::fs::write(&meta_path, &metadata).is_ok();

    let effective_prompt = prompt.unwrap_or(DEFAULT_DIFF_PROMPT);
    let mut gia_args: Vec<String> = vec![effective_prompt.to_string()];
    if has_meta {
        gia_args.push("-f".to_string());
        gia_args.push(meta_path.to_string_lossy().into_owned());
    }

    let mut gia = match std::process::Command::new("gia")
        .args(&gia_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return format!("Error starting gia: {e}"),
    };

    if let Some(mut stdin) = gia.stdin.take() {
        let _ = stdin.write_all(&diff);
    }

    let result = match gia.wait_with_output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if stdout.is_empty() {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            } else {
                stdout
            }
        }
        Err(e) => format!("Error waiting for gia: {e}"),
    };

    if has_meta {
        let _ = std::fs::remove_file(&meta_path);
    }

    result
}

fn run_gia_log(repo_path: &str, sha1: &str, sha2: &str, prompt: Option<&str>) -> String {
    let base = match resolve_diff_base(repo_path, sha1, sha2) {
        Ok(b) => b,
        Err(e) => return format!("Error resolving log base: {e}"),
    };

    let log_range = format!("{}..{}", base, sha2);
    let log_out = match std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            GIT_LOG_METADATA_FORMAT,
            "--name-status",
            &log_range,
        ])
        .output()
    {
        Ok(out) => out,
        Err(e) => return format!("Error running git log: {e}"),
    };
    if !log_out.status.success() {
        let stderr = String::from_utf8_lossy(&log_out.stderr).trim().to_string();
        return format!("git log exited with {}: {}", log_out.status, stderr);
    }
    if log_out.stdout.is_empty() {
        return "No commits found in this range.".to_string();
    }

    let effective_prompt = prompt.unwrap_or(DEFAULT_LOG_PROMPT);
    let mut gia = match std::process::Command::new("gia")
        .arg(effective_prompt)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => return format!("Error starting gia: {e}"),
    };

    if let Some(mut stdin) = gia.stdin.take() {
        let _ = stdin.write_all(&log_out.stdout);
    }

    match gia.wait_with_output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if stdout.is_empty() {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            } else {
                stdout
            }
        }
        Err(e) => format!("Error waiting for gia: {e}"),
    }
}

fn run_gia_log_browser(repo_path: &str, sha1: &str, sha2: &str, prompt: Option<&str>) {
    let base = match resolve_diff_base(repo_path, sha1, sha2) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Error resolving log base: {e}");
            return;
        }
    };

    let log_range = format!("{}..{}", base, sha2);
    let log_out = match std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            GIT_LOG_METADATA_FORMAT,
            "--name-status",
            &log_range,
        ])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            eprintln!("git log error: {e}");
            return;
        }
    };

    let effective_prompt = prompt.unwrap_or(DEFAULT_LOG_PROMPT);
    let mut gia = match std::process::Command::new("gia")
        .args(["-b", effective_prompt])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(e) => {
            eprintln!("gia error: {e}");
            return;
        }
    };

    if let Some(mut stdin) = gia.stdin.take() {
        let _ = stdin.write_all(&log_out.stdout);
    }
    let _ = gia.wait();
}

fn serve_git_log(repo_path: &str, sha1: &str, sha2: &str) -> String {
    let base = match resolve_diff_base(repo_path, sha1, sha2) {
        Ok(b) => b,
        Err(e) => return format!("<pre>Error resolving log base: {}</pre>", html_escape(&e)),
    };

    let log_range = format!("{}..{}", base, sha2);
    let out = match std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            "--pretty=format:commit %H%nAuthor: %an <%ae>%nDate:   %ci%nRefs:   %D%n%n    %s%n%n    %b",
            &log_range,
        ])
        .output()
    {
        Ok(o) => o,
        Err(e) => {
            return format!(
                "<pre>Error running git log: {}</pre>",
                html_escape(&e.to_string())
            )
        }
    };

    let text = if out.stdout.is_empty() {
        "No commits found in this range.".to_string()
    } else {
        String::from_utf8_lossy(&out.stdout).to_string()
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Git Log</title>
<style>
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{
    font-family: "Cascadia Code", "Consolas", "Courier New", monospace;
    background: #0f1117;
    color: #e2e8f0;
    padding: 24px;
    font-size: 13px;
    line-height: 1.6;
  }}
  h1 {{ font-size: 15px; color: #63b3ed; margin-bottom: 16px; font-family: "Segoe UI", sans-serif; }}
  .shas {{
    font-size: 12px;
    color: #718096;
    margin-bottom: 20px;
    display: flex;
    gap: 8px;
    align-items: center;
    font-family: "Segoe UI", sans-serif;
  }}
  .sha {{ background: #2d3748; padding: 2px 8px; border-radius: 4px; color: #a0aec0; }}
  .arrow {{ color: #4a5568; }}
  pre {{
    white-space: pre-wrap;
    word-break: break-all;
    color: #cbd5e0;
  }}
</style>
</head>
<body>
<h1>Git Log</h1>
<div class="shas">
  <span class="sha">{sha1}</span>
  <span class="arrow">&#8594;</span>
  <span class="sha">{sha2}</span>
</div>
<pre>{log}</pre>
</body>
</html>"#,
        sha1 = &sha1[..sha1.len().min(7)],
        sha2 = &sha2[..sha2.len().min(7)],
        log = html_escape(&text),
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn build_html(sha1: &str, sha2: &str, summary: &str) -> String {
    let summary_escaped = html_escape(summary);
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Diff Summary</title>
<style>
  * {{ box-sizing: border-box; margin: 0; padding: 0; }}
  body {{
    font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    background: #0f1117;
    color: #e2e8f0;
    display: flex;
    justify-content: center;
    align-items: flex-start;
    min-height: 100vh;
    padding: 32px 16px;
  }}
  .card {{
    background: #1a1f2e;
    border: 1px solid #2d3748;
    border-radius: 12px;
    padding: 32px 40px;
    max-width: 960px;
    width: 100%;
    box-shadow: 0 20px 60px rgba(0,0,0,0.5);
  }}
  h1 {{ font-size: 18px; color: #63b3ed; margin-bottom: 16px; }}
  .shas {{
    font-family: monospace;
    font-size: 12px;
    color: #718096;
    margin-bottom: 24px;
    display: flex;
    gap: 8px;
    align-items: center;
  }}
  .sha {{ background: #2d3748; padding: 2px 8px; border-radius: 4px; color: #a0aec0; }}
  .arrow {{ color: #4a5568; }}
  .summary {{
    line-height: 1.7;
    color: #e2e8f0;
    white-space: pre-wrap;
    font-size: 13px;
    font-family: "Segoe UI", ui-sans-serif, sans-serif;
  }}
</style>
</head>
<body>
<div class="card">
  <h1>AI Diff Summary</h1>
  <div class="shas">
    <span class="sha">{sha1}</span>
    <span class="arrow">&#8594;</span>
    <span class="sha">{sha2}</span>
  </div>
  <div class="summary">{summary}</div>
</div>
</body>
</html>"#,
        sha1 = sha1,
        sha2 = sha2,
        summary = summary_escaped,
    )
}

fn send_response(stream: &mut TcpStream, status: u16, content_type: &str, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        _ => "Error",
    };
    let response = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {ct}\r\nContent-Length: {len}\r\nConnection: close\r\n\r\n{body}",
        status = status,
        reason = reason,
        ct = content_type,
        len = body.len(),
        body = body,
    );
    let _ = stream.write_all(response.as_bytes());
}
