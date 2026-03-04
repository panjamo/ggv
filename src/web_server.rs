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

pub fn base_url(port: u16) -> String {
    format!("http://[::1]:{}", port)
}

/// Binds to the given port (0 = OS-assigned) and spawns the server thread.
/// Returns the join handle and the actual bound port.
pub fn start(
    port: u16,
    repo_path: String,
    use_ai: bool,
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
    let handle =
        std::thread::spawn(move || run_server(listener, &repo_path, use_ai, gia_browser, prompt));
    Ok((handle, actual_port))
}

fn run_server(
    listener: TcpListener,
    repo_path: &str,
    use_ai: bool,
    gia_browser: bool,
    prompt: Option<String>,
) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let repo_clone = repo_path.to_string();
                let prompt_clone = prompt.clone();
                std::thread::spawn(move || {
                    handle_connection(stream, &repo_clone, use_ai, gia_browser, prompt_clone)
                });
            }
            Err(e) => eprintln!("Connection error: {}", e),
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    repo_path: &str,
    use_ai: bool,
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

    if path != "/diff" {
        send_response(&mut stream, 404, "text/plain", "Not Found");
        return;
    }

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

    if !use_ai {
        run_git_difftool(repo_path, &sha1, &sha2);
        send_response(
            &mut stream,
            200,
            "text/html; charset=utf-8",
            "<html><body><script>window.close();</script></body></html>",
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
            "<html><body><script>window.close();</script></body></html>",
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

fn is_valid_sha(s: &str) -> bool {
    s.len() >= 7 && s.len() <= 40 && s.chars().all(|c| c.is_ascii_hexdigit())
}

fn parse_query(query: &str) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for pair in query.split('&') {
        if let Some((k, v)) = pair.split_once('=') {
            map.insert(k.to_string(), v.to_string());
        }
    }
    map
}

fn run_git_difftool(repo_path: &str, sha1: &str, sha2: &str) {
    let _ = std::process::Command::new("git")
        .args(["-C", repo_path, "difftool", "-d", sha1, sha2])
        .spawn();
}

fn run_gia_browser(repo_path: &str, sha1: &str, sha2: &str, prompt: Option<&str>) {
    let diff = match std::process::Command::new("git")
        .args(["-C", repo_path, "diff", sha1, sha2])
        .output()
    {
        Ok(out) => out.stdout,
        Err(e) => {
            eprintln!("git diff error: {e}");
            return;
        }
    };

    let effective_prompt = prompt.unwrap_or(DEFAULT_BROWSER_PROMPT);

    let mut gia = match std::process::Command::new("gia")
        .args(["-b", "-c", effective_prompt])
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
}

fn run_gia_diff(repo_path: &str, sha1: &str, sha2: &str, prompt: Option<&str>) -> String {
    let diff = match std::process::Command::new("git")
        .args(["-C", repo_path, "diff", sha1, sha2])
        .output()
    {
        Ok(out) => out.stdout,
        Err(e) => return format!("Error running git diff: {e}"),
    };

    if diff.is_empty() {
        return "No differences found between these commits.".to_string();
    }

    let effective_prompt = prompt.unwrap_or(DEFAULT_DIFF_PROMPT);

    let mut gia = match std::process::Command::new("gia")
        .args(["-c", effective_prompt])
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
