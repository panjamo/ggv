use std::collections::HashMap;
use std::io::{BufRead, BufReader, Write as IoWrite};
use std::net::{IpAddr, Ipv6Addr, SocketAddr, TcpListener, TcpStream};
use std::process::Stdio;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use pulldown_cmark::{html, Options, Parser as MdParser};

use crate::filter::RefFilter;
use crate::graph::GitGraphviz;
use crate::graphviz::generate_svg;
use crate::theme::Theme;

/// All parameters needed to regenerate the DOT+SVG after a git operation.
pub struct RegenerateConfig {
    pub repo_path: String,
    pub dot_path: String,
    pub filter: String,
    pub gitlab_url: Option<String>,
    pub from_commit: Option<String>,
    pub theme: Theme,
    pub current_branch_only: bool,
    pub no_fetch: bool,
    pub keep_dot: bool,
    /// Filled in by `start()` once the port is known.
    pub web_server_url: String,
}

fn regenerate(config: &RegenerateConfig) {
    if !config.no_fetch {
        let _ = std::process::Command::new("git")
            .args(["-C", &config.repo_path, "fetch", "--tags", "--prune"])
            .status();
    }
    let filter = RefFilter::from_string(&config.filter);
    let git_viz = match GitGraphviz::new(
        &config.repo_path,
        filter,
        config.gitlab_url.clone(),
        config.from_commit.clone(),
        config.theme,
        config.current_branch_only,
    ) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("Regenerate: failed to open repo: {e}");
            return;
        }
    };
    if let Err(e) = git_viz.generate_dot(&config.dot_path) {
        eprintln!("Regenerate: failed to generate DOT: {e}");
        return;
    }
    let ws_url = if config.web_server_url.is_empty() {
        None
    } else {
        Some(config.web_server_url.as_str())
    };
    let repo_name = crate::utils::repo_name_from_path(&config.repo_path);
    match generate_svg(&config.dot_path, git_viz.forge_url(), ws_url, &repo_name) {
        Ok(_) => {
            if !config.keep_dot {
                let _ = std::fs::remove_file(&config.dot_path);
            }
            eprintln!("SVG regenerated.");
        }
        Err(e) => eprintln!("Regenerate: SVG generation failed: {e}"),
    }
}

const DEFAULT_DIFF_PROMPT: &str = "You are an experienced software engineer performing a PR review.

Summarize the following Git diff and commit message in an ultra-compact way.

Rules:
- Maximum 1–6 bullet points
- Focus on the intent and impact of the changes, not a description of what lines were added or removed
- Answer: why was this change made, what problem does it solve, what is the effect on the system?
- Mention affected components and possible breaking changes
- No explanations for beginners
- No introduction or filler text
- Write technically and directly, like a PR review comment

Format:
- <component / area>: <impact or intent of change>
";

const DEFAULT_LOG_PROMPT: &str = DEFAULT_DIFF_PROMPT;

/// Returns the diff prompt: reads `~/.ggv/prompt/default_prompt.md` if it exists,
/// otherwise writes the built-in default there and returns it.
fn load_diff_prompt() -> String {
    if let Some(home) = dirs::home_dir() {
        let prompt_path = home.join(".ggv").join("prompt").join("default_prompt.md");
        if prompt_path.exists() {
            if let Ok(content) = std::fs::read_to_string(&prompt_path) {
                return content;
            }
        } else if let Some(parent) = prompt_path.parent() {
            let _ = std::fs::create_dir_all(parent);
            let _ = std::fs::write(&prompt_path, DEFAULT_DIFF_PROMPT);
        }
    }
    DEFAULT_DIFF_PROMPT.to_string()
}

/// Git log format used as metadata context when feeding diffs to the AI.
const GIT_LOG_METADATA_FORMAT: &str =
    "--pretty=format:commit %h%nRefs: %D%nAuthor: %an <%ae>%nDate: %ci%nSubject: %s%n";

/// URL-encoded text shown in gia's audio recording dialog.
const AUDIO_DIALOG_TEXT: &str =
    "A%20Git%20diff%20is%20being%20analyzed%20by%20AI.%0A\
     Your%20recording%20extends%20the%20prompt%20-%20use%20it%20to%20guide%20the%20analysis%3A%0A%0A\
     -%20Focus%20on%20specific%20files%20or%20modules%0A\
     -%20Request%20a%20brief%20summary%20instead%20of%20full%20analysis%0A\
     -%20Ignore%20test%20files%20or%20certain%20areas%0A\
     -%20Ask%20for%20a%20risk%20assessment%20or%20improvement%20suggestions%0A\
     -%20Set%20the%20output%20language%2C%20e.g.%20respond%20in%20German";

pub fn base_url(port: u16) -> String {
    format!("http://[::1]:{}", port)
}

/// Appends a language instruction to a prompt string.
fn with_lang(prompt: &str, lang: &str) -> String {
    format!("{}\nRespond in the language of locale: {}.", prompt, lang)
}

/// Appends a voice-input instruction to a prompt string when audio mode is active.
fn with_audio(prompt: &str) -> String {
    format!(
        "{}\nThe audio/ogg.attachment is an extension for the prompt \
It may contain filter instructions or directions — \
for example, specifying what should or should not be considered in the analysis.",
        prompt
    )
}

/// Binds to the given port (0 = OS-assigned) and spawns the server thread.
/// Returns the join handle and the actual bound port.
/// Seconds without a heartbeat before GGV shuts itself down.
/// Only triggers after the first heartbeat has been received.
const HEARTBEAT_TIMEOUT_SECS: u64 = 300;
/// Interval at which the watchdog checks the heartbeat timestamp.
const WATCHDOG_INTERVAL_SECS: u64 = 10;

#[allow(clippy::too_many_arguments)]
pub fn start(
    port: u16,
    repo_path: String,
    svg_path: String,
    prompt: Option<String>,
    lang: String,
    gia_audio: bool,
    theme: Theme,
    mut regen: Option<RegenerateConfig>,
    max_diff_files: usize,
) -> anyhow::Result<(std::thread::JoinHandle<()>, u16)> {
    let addr = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), port);
    let listener = TcpListener::bind(addr)?;
    let actual_port = listener.local_addr()?.port();
    eprintln!(
        "Diff server listening on http://[::1]:{} (Ctrl+C to stop)",
        actual_port
    );
    if let Some(ref mut cfg) = regen {
        cfg.web_server_url = base_url(actual_port);
    }
    let regen = regen.map(Arc::new);

    // Heartbeat: 0 = no heartbeat received yet; otherwise UNIX timestamp of last ping.
    let last_hb: Arc<AtomicU64> = Arc::new(AtomicU64::new(0));

    // Watchdog thread: exits the process if the page has been closed.
    let last_hb_watchdog = last_hb.clone();
    std::thread::spawn(move || loop {
        std::thread::sleep(std::time::Duration::from_secs(WATCHDOG_INTERVAL_SECS));
        let ts = last_hb_watchdog.load(Ordering::Relaxed);
        if ts > 0 {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now.saturating_sub(ts) >= HEARTBEAT_TIMEOUT_SECS {
                eprintln!("No heartbeat for {HEARTBEAT_TIMEOUT_SECS}s — shutting down.");
                std::process::exit(0);
            }
        }
    });

    let handle = std::thread::spawn(move || {
        run_server(
            listener,
            &repo_path,
            &svg_path,
            prompt,
            &lang,
            gia_audio,
            theme,
            regen,
            last_hb,
            max_diff_files,
        )
    });
    Ok((handle, actual_port))
}

#[allow(clippy::too_many_arguments)]
fn run_server(
    listener: TcpListener,
    repo_path: &str,
    svg_path: &str,
    prompt: Option<String>,
    lang: &str,
    gia_audio: bool,
    theme: Theme,
    regen: Option<Arc<RegenerateConfig>>,
    last_hb: Arc<AtomicU64>,
    max_diff_files: usize,
) {
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                let repo_clone = repo_path.to_string();
                let svg_clone = svg_path.to_string();
                let prompt_clone = prompt.clone();
                let lang_clone = lang.to_string();
                let regen_clone = regen.clone();
                let last_hb_clone = last_hb.clone();
                std::thread::spawn(move || {
                    handle_connection(
                        stream,
                        &repo_clone,
                        &svg_clone,
                        prompt_clone,
                        &lang_clone,
                        gia_audio,
                        theme,
                        regen_clone,
                        last_hb_clone,
                        max_diff_files,
                    )
                });
            }
            Err(e) => eprintln!("Connection error: {}", e),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn handle_connection(
    mut stream: TcpStream,
    repo_path: &str,
    svg_path: &str,
    prompt: Option<String>,
    lang: &str,
    gia_audio: bool,
    theme: Theme,
    regen: Option<Arc<RegenerateConfig>>,
    last_hb: Arc<AtomicU64>,
    max_diff_files: usize,
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
        "/heartbeat" => {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            last_hb.store(now, Ordering::Relaxed);
            send_response(&mut stream, 200, "text/plain", "OK");
        }
        "/view" => {
            let repo_name = crate::utils::repo_name_from_path(repo_path);
            serve_svg(&mut stream, svg_path, &repo_name);
        }
        "/version" => {
            let version = svg_mtime(svg_path);
            send_response(&mut stream, 200, "text/plain", &version);
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
            if let Some(cfg) = regen {
                std::thread::spawn(move || regenerate(&cfg));
            }
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
            if let Some(cfg) = regen {
                std::thread::spawn(move || regenerate(&cfg));
            }
        }
        "/delete-tag" => {
            let params = parse_query(query);
            let name = match params.get("name") {
                Some(n) if is_valid_ref_name(n) => n.clone(),
                _ => {
                    send_response(&mut stream, 400, "text/plain", "Invalid or missing 'name'");
                    return;
                }
            };
            run_tag_delete(repo_path, &name);
            send_response(&mut stream, 200, "text/plain", "OK");
            if let Some(cfg) = regen {
                std::thread::spawn(move || regenerate(&cfg));
            }
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
            let include_log = !params.get("nolog").map(|v| v == "1").unwrap_or(false);
            let filter_str = params.get("filter").cloned().unwrap_or_default();
            let pathspecs = parse_pathspec(&filter_str);

            if !force_ai {
                if has_git_diff(repo_path, &sha1, &sha2, &[]) {
                    run_git_difftool(repo_path, &sha1, &sha2);
                } else {
                    send_response(
                        &mut stream,
                        200,
                        "text/html; charset=utf-8",
                        &build_no_diff_html(&sha1, &sha2, theme),
                    );
                    return;
                }
            }

            if !force_ai {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    "<html><body><script>window.close();</script></body></html>",
                );
                return;
            }

            if !has_git_diff(repo_path, &sha1, &sha2, &pathspecs) {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    &build_no_diff_html(&sha1, &sha2, theme),
                );
                return;
            }
            let loaded_prompt = load_diff_prompt();
            let base_prompt = prompt.as_deref().unwrap_or(&loaded_prompt).to_string();
            let effective_prompt = with_lang(&base_prompt, lang);
            let effective_prompt = if gia_audio {
                with_audio(&effective_prompt)
            } else {
                effective_prompt
            };
            let summary = run_gia_diff(
                repo_path,
                &sha1,
                &sha2,
                Some(&effective_prompt),
                include_log,
                gia_audio,
                &pathspecs,
            );
            if summary.is_empty() {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    "<script>window.close();</script>",
                );
                return;
            }
            let diff_section = diff2html_section(
                repo_path,
                &sha1,
                &sha2,
                theme,
                &pathspecs,
                &filter_str,
                max_diff_files,
            )
            .ok();
            let html = build_html(
                &sha1[..sha1.len().min(7)],
                &sha2[..sha2.len().min(7)],
                &summary,
                theme,
                diff_section,
            );
            send_response(&mut stream, 200, "text/html; charset=utf-8", &html);
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
            let filter_str = params.get("filter").cloned().unwrap_or_default();
            let pathspecs = parse_pathspec(&filter_str);
            if !has_git_diff(repo_path, &sha1, &sha2, &[]) {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    &build_no_diff_html(&sha1, &sha2, theme),
                );
                return;
            }
            let base_prompt = prompt.as_deref().unwrap_or(DEFAULT_LOG_PROMPT).to_string();
            let effective_prompt = with_lang(&base_prompt, lang);
            let effective_prompt = if gia_audio {
                with_audio(&effective_prompt)
            } else {
                effective_prompt
            };
            let summary = run_gia_log(repo_path, &sha1, &sha2, Some(&effective_prompt), gia_audio);
            if summary.is_empty() {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    "<script>window.close();</script>",
                );
                return;
            }
            let diff_section = diff2html_section(
                repo_path,
                &sha1,
                &sha2,
                theme,
                &pathspecs,
                &filter_str,
                max_diff_files,
            )
            .ok();
            let html = build_html(
                &sha1[..sha1.len().min(7)],
                &sha2[..sha2.len().min(7)],
                &summary,
                theme,
                diff_section,
            );
            send_response(&mut stream, 200, "text/html; charset=utf-8", &html);
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
            if !has_git_diff(repo_path, &sha1, &sha2, &[]) {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    &build_no_diff_html(&sha1, &sha2, theme),
                );
                return;
            }
            let html = serve_git_log(repo_path, &sha1, &sha2, theme);
            send_response(&mut stream, 200, "text/html; charset=utf-8", &html);
        }
        "/diff2html" => {
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
            let filter_str = params.get("filter").cloned().unwrap_or_default();
            let pathspecs = parse_pathspec(&filter_str);
            if !has_git_diff(repo_path, &sha1, &sha2, &pathspecs) {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    &build_no_diff_html(&sha1, &sha2, theme),
                );
                return;
            }
            let gitlab_url = regen.as_ref().and_then(|r| r.gitlab_url.as_deref());
            match run_diff2html(
                repo_path,
                &sha1,
                &sha2,
                theme,
                &pathspecs,
                &filter_str,
                max_diff_files,
                gitlab_url,
            ) {
                Ok(html) => send_response(&mut stream, 200, "text/html; charset=utf-8", &html),
                Err(e) => send_response(&mut stream, 500, "text/plain", &e),
            }
        }
        "/diff2html-single" => {
            let params = parse_query(query);
            let commit = match params.get("commit") {
                Some(s) if is_valid_sha(s) => s.clone(),
                _ => {
                    send_response(
                        &mut stream,
                        400,
                        "text/plain",
                        "Invalid or missing 'commit'",
                    );
                    return;
                }
            };
            // Resolve the parent commit hash
            let parent = std::process::Command::new("git")
                .args(["-C", repo_path, "log", "-1", "--pretty=%P", &commit])
                .output()
                .ok()
                .and_then(|o| {
                    let s = String::from_utf8_lossy(&o.stdout).trim().to_string();
                    // Take first parent if there are multiple
                    s.split_whitespace()
                        .next()
                        .map(|p| p[..p.len().min(40)].to_string())
                });
            let (sha1, sha2) = match parent {
                Some(p) if !p.is_empty() => (p, commit),
                // Root commit (no parent): diff against empty tree
                _ => (
                    "4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string(),
                    commit,
                ),
            };
            let filter_str = params.get("filter").cloned().unwrap_or_default();
            let pathspecs = parse_pathspec(&filter_str);
            if !has_git_diff(repo_path, &sha1, &sha2, &pathspecs) {
                send_response(
                    &mut stream,
                    200,
                    "text/html; charset=utf-8",
                    &build_no_diff_html(&sha1, &sha2, theme),
                );
                return;
            }
            let older = if sha1 == "4b825dc642cb6eb9a060e54bf8d69288fbee4904" {
                None
            } else {
                Some(sha1.clone())
            };
            let newer = find_child_commit(repo_path, &sha2);
            let gitlab_url = regen.as_ref().and_then(|r| r.gitlab_url.as_deref());
            match run_diff2html(
                repo_path,
                &sha1,
                &sha2,
                theme,
                &pathspecs,
                &filter_str,
                max_diff_files,
                gitlab_url,
            ) {
                Ok(html) => {
                    let html = inject_commit_navigation(&html, older.as_deref(), newer.as_deref());
                    send_response(&mut stream, 200, "text/html; charset=utf-8", &html);
                }
                Err(e) => send_response(&mut stream, 500, "text/plain", &e),
            }
        }
        _ => {
            send_response(&mut stream, 404, "text/plain", "Not Found");
        }
    }
}

fn svg_mtime(svg_path: &str) -> String {
    std::fs::metadata(svg_path)
        .and_then(|m| m.modified())
        .map(|t| {
            t.duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_millis()
                .to_string()
        })
        .unwrap_or_else(|_| "0".to_string())
}

fn serve_svg(stream: &mut TcpStream, svg_path: &str, repo_name: &str) {
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
    // Inline SVG favicon: three commits (green main branch) + one branch commit (blue)
    let favicon = "data:image/svg+xml,%3Csvg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 16 16'%3E%3Cline x1='5' y1='1' x2='5' y2='15' stroke='%234ade80' stroke-width='1.5'/%3E%3Cline x1='5' y1='5' x2='11' y2='11' stroke='%2360a5fa' stroke-width='1.5'/%3E%3Ccircle cx='5' cy='2' r='2' fill='%234ade80'/%3E%3Ccircle cx='5' cy='6' r='2' fill='%234ade80'/%3E%3Ccircle cx='5' cy='14' r='2' fill='%234ade80'/%3E%3Ccircle cx='11' cy='11' r='2' fill='%2360a5fa'/%3E%3C/svg%3E";
    let html = format!(
        r#"<!DOCTYPE html>
<html>
<head>
<meta charset="utf-8">
<title>⎇ {repo_name}</title>
<link rel="icon" type="image/svg+xml" href="{favicon}">
<style>
  body {{ margin: 0; background: #1a1f2e; overflow: auto; }}
  svg {{ display: block; }}
</style>
<script>
(function() {{
  var v = null;
  function poll() {{
    fetch('/version').then(function(r) {{ return r.text(); }}).then(function(nv) {{
      if (v === null) {{ v = nv; }}
      else if (nv !== v) {{ location.reload(); }}
    }}).catch(function() {{}});
  }}
  setInterval(poll, 1500);
  poll();
  setInterval(function() {{ fetch('/heartbeat').catch(function(){{}}); }}, 2000);
}})();
</script>
</head>
<body>{}
<div id="ggv-flt-bar" style="position:fixed;top:10px;right:10px;z-index:1000;display:flex;align-items:center;gap:6px;background:#1a1f2e;border:1px solid #2d3748;border-radius:6px;padding:5px 10px;font-family:'Segoe UI',sans-serif;font-size:12px;">
  <span style="color:#718096;white-space:nowrap;">Filter:</span>
  <input id="ggv-flt-inp" type="text" placeholder="*.cpp *.h" style="width:160px;padding:3px 6px;border:1px solid #2d3748;border-radius:4px;background:#0f1117;color:#e2e8f0;font-family:monospace;font-size:11px;">
  <button onclick="ggvSave()" style="padding:2px 8px;border-radius:4px;cursor:pointer;border:1px solid #2d3748;background:#1a1f2e;color:#e2e8f0;font-size:11px;">Set</button>
  <button onclick="ggvClear()" style="padding:2px 8px;border-radius:4px;cursor:pointer;border:1px solid #2d3748;background:#1a1f2e;color:#718096;font-size:11px;">✕</button>
  <span id="ggv-flt-ind" style="color:#f6ad55;font-size:10px;display:none;">●</span>
  <button onclick="ggvHelp()" title="Help  ?" style="padding:2px 8px;border-radius:4px;cursor:pointer;border:1px solid #2d3748;background:#1a1f2e;color:#a0aec0;font-size:12px;font-family:monospace;line-height:1.4;" onmouseover="this.style.borderColor='#63b3ed';this.style.color='#63b3ed'" onmouseout="this.style.borderColor='#2d3748';this.style.color='#a0aec0'">?</button>
</div>
<!-- ── Help overlay ── -->
<div id="ggv-help-overlay" onclick="if(event.target===this)ggvHelp()" style="display:none;position:fixed;inset:0;background:rgba(0,0,0,0.65);z-index:10000;align-items:flex-start;justify-content:center;padding:24px 16px;overflow-y:auto;">
  <div style="background:#1a1f2e;border:1px solid #2d3748;border-radius:14px;padding:28px 32px;width:100%;max-width:660px;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;box-shadow:0 20px 60px rgba(0,0,0,.8);color:#e2e8f0;">
    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:22px;">
      <span style="font-size:16px;font-weight:700;color:#63b3ed;">&#9432; GGV Help</span>
      <button onclick="ggvHelp()" style="background:none;border:none;color:#718096;font-size:20px;cursor:pointer;line-height:1;padding:0 4px;">&#x2715;</button>
    </div>

    <!-- Edge labels -->
    <div style="margin-bottom:18px;">
      <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.07em;color:#4a5568;margin-bottom:10px;">Edge Labels (blue numbers on arrows)</div>
      <table style="border-collapse:collapse;width:100%;font-size:13px;">
        <tr><td style="padding:4px 0 4px 0;width:56%;color:#a0aec0;">Number value</td><td style="padding:4px 0;color:#cbd5e1;">Number of commits condensed into that edge</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Font size</td><td style="padding:4px 0;color:#cbd5e1;">Proportional to the number of changed files in that range</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Hover edge label</td><td style="padding:4px 0;color:#cbd5e1;">Tooltip listing the changed files</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Left-click edge label</td><td style="padding:4px 0;color:#cbd5e1;">Opens <code style="background:#2d3748;border-radius:3px;padding:1px 5px;font-size:11px;">git difftool</code> for that range</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Right-click edge label</td><td style="padding:4px 0;color:#cbd5e1;">Context menu: AI diff+log, AI diff-only, AI log-only</td></tr>
      </table>
    </div>

    <!-- Edges -->
    <div style="margin-bottom:18px;">
      <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.07em;color:#4a5568;margin-bottom:10px;">Edges (arrows between commits)</div>
      <table style="border-collapse:collapse;width:100%;font-size:13px;">
        <tr><td style="padding:4px 0;width:56%;color:#a0aec0;">Hover edge</td><td style="padding:4px 0;color:#cbd5e1;">Tooltip listing all commits condensed into that range</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Click edge</td><td style="padding:4px 0;color:#cbd5e1;">Opens the GitLab / GitHub compare view for that range</td></tr>
      </table>
    </div>

    <!-- Commit nodes -->
    <div style="margin-bottom:18px;">
      <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.07em;color:#4a5568;margin-bottom:10px;">Commit Nodes</div>
      <table style="border-collapse:collapse;width:100%;font-size:13px;">
        <tr><td style="padding:4px 0;width:56%;color:#a0aec0;">Click node</td><td style="padding:4px 0;color:#cbd5e1;">Copies the full 40-character SHA to the clipboard (amber flash)</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Drag node onto another</td><td style="padding:4px 0;color:#cbd5e1;">Opens the forge compare view — always corrected to older&#8594;newer</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Ctrl + drag onto another</td><td style="padding:4px 0;color:#cbd5e1;">Opens the web server diff view for that range</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Hover SVG background</td><td style="padding:4px 0;color:#cbd5e1;">Tooltip: repo name, current branch, HEAD commit, author, date</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;vertical-align:top;">Right-click node</td><td style="padding:4px 0;color:#cbd5e1;">Context menu (see below)</td></tr>
      </table>
    </div>

    <!-- Node colors -->
    <div style="margin-bottom:18px;">
      <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.07em;color:#4a5568;margin-bottom:10px;">Node Colors</div>
      <table style="border-collapse:collapse;width:100%;font-size:13px;">
        <tr><td style="padding:4px 0;width:56%;"><span style="background:#b7791f;color:#fffbeb;border-radius:4px;padding:1px 8px;font-size:11px;">yellow</span></td><td style="padding:4px 0;color:#cbd5e1;">Current checkout (HEAD)</td></tr>
        <tr><td style="padding:4px 0;"><span style="background:#1e3a5f;color:#63b3ed;border-radius:4px;padding:1px 8px;font-size:11px;">blue</span></td><td style="padding:4px 0;color:#cbd5e1;">Local branch tip</td></tr>
        <tr><td style="padding:4px 0;"><span style="background:#1c4532;color:#68d391;border-radius:4px;padding:1px 8px;font-size:11px;">green</span></td><td style="padding:4px 0;color:#cbd5e1;">Remote-tracking branch tip</td></tr>
        <tr><td style="padding:4px 0;"><span style="background:#521b41;color:#fbb6ce;border-radius:4px;padding:1px 8px;font-size:11px;">pink</span></td><td style="padding:4px 0;color:#cbd5e1;">Tag</td></tr>
        <tr><td style="padding:4px 0;"><span style="background:#744210;color:#fbd38d;border-radius:4px;padding:1px 8px;font-size:11px;">orange</span></td><td style="padding:4px 0;color:#cbd5e1;">Other ref (e.g. stash, notes)</td></tr>
        <tr><td style="padding:4px 0;"><span style="background:#1a1f2e;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:1px 8px;font-size:11px;">dark</span></td><td style="padding:4px 0;color:#cbd5e1;">Merge junction or root commit</td></tr>
      </table>
    </div>

    <!-- Right-click context menu -->
    <div style="margin-bottom:18px;">
      <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.07em;color:#4a5568;margin-bottom:10px;">Right-Click Context Menu on Commit Nodes</div>
      <table style="border-collapse:collapse;width:100%;font-size:13px;">
        <tr><td style="padding:4px 0;width:56%;color:#a0aec0;">Select / Change first commit</td><td style="padding:4px 0;color:#cbd5e1;">Pin a commit as the start of a manual compare range</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Compare with &lt;sha&gt;…</td><td style="padding:4px 0;color:#cbd5e1;">Open diff view between pinned and this commit</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Compare with AI…</td><td style="padding:4px 0;color:#cbd5e1;">AI summary (diff + log metadata) for the pinned range</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Compare with AI diff…</td><td style="padding:4px 0;color:#cbd5e1;">AI summary — diff only, no commit log</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Compare with AI log…</td><td style="padding:4px 0;color:#cbd5e1;">AI summary — commit log only, no diff</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Show Git Log…</td><td style="padding:4px 0;color:#cbd5e1;">Formatted commit list between pinned and this commit</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Copy SHA</td><td style="padding:4px 0;color:#cbd5e1;">Copies full 40-character commit hash</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Copy branch / tag</td><td style="padding:4px 0;color:#cbd5e1;">Copies the branch or tag name at this commit</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Delete local / remote branch</td><td style="padding:4px 0;color:#cbd5e1;">Force-deletes branch after confirmation</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Delete Tag (local + remote)</td><td style="padding:4px 0;color:#cbd5e1;">Removes tag locally and from all remotes after confirmation</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Checkout branch</td><td style="padding:4px 0;color:#cbd5e1;">Runs <code style="background:#2d3748;border-radius:3px;padding:1px 5px;font-size:11px;">git checkout &lt;branch&gt;</code></td></tr>
      </table>
    </div>

    <!-- Diff view -->
    <div style="margin-bottom:18px;">
      <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.07em;color:#4a5568;margin-bottom:10px;">Diff / Commit View (opens in new tab)</div>
      <table style="border-collapse:collapse;width:100%;font-size:13px;">
        <tr><td style="padding:4px 0;width:56%;color:#a0aec0;">Commit cards</td><td style="padding:4px 0;color:#cbd5e1;">Each card shows hash, author, age, branch/tag badges, subject, and number of changed files</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Click commit hash</td><td style="padding:4px 0;color:#cbd5e1;">Opens the single-commit diff view for that commit</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Right-click commit card</td><td style="padding:4px 0;color:#cbd5e1;">Context menu to pin commit or compare with pinned</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Click file header</td><td style="padding:4px 0;color:#cbd5e1;">Collapse / expand that file&#39;s diff</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">File filter bar</td><td style="padding:4px 0;color:#cbd5e1;">Filter visible files by glob pattern (e.g. <code style="background:#2d3748;border-radius:3px;padding:1px 5px;font-size:11px;">*.rs *.toml</code>); persisted across views</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:1px 7px;font-size:11px;">[</kbd></td><td style="padding:4px 0;color:#cbd5e1;">Navigate to older (parent) commit</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:1px 7px;font-size:11px;">]</kbd></td><td style="padding:4px 0;color:#cbd5e1;">Navigate to newer (child) commit</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:1px 7px;font-size:11px;">?</kbd></td><td style="padding:4px 0;color:#cbd5e1;">Toggle keyboard shortcut overlay</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Diff suppressed notice</td><td style="padding:4px 0;color:#cbd5e1;">Shown when changed files exceed the <code style="background:#2d3748;border-radius:3px;padding:1px 5px;font-size:11px;">-M</code> limit (default 100); raise with <code style="background:#2d3748;border-radius:3px;padding:1px 5px;font-size:11px;">-M 0</code> to disable</td></tr>
      </table>
    </div>

    <!-- Filter bar -->
    <div style="margin-bottom:18px;">
      <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.07em;color:#4a5568;margin-bottom:10px;">Filter Bar (top-right of this page)</div>
      <table style="border-collapse:collapse;width:100%;font-size:13px;">
        <tr><td style="padding:4px 0;width:56%;color:#a0aec0;">Filter input</td><td style="padding:4px 0;color:#cbd5e1;">Glob patterns to pre-filter diff views (e.g. <code style="background:#2d3748;border-radius:3px;padding:1px 5px;font-size:11px;">src/ *.cs</code>)</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Orange dot ●</td><td style="padding:4px 0;color:#cbd5e1;">Indicates an active filter is applied to diff views</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">Set / Enter</td><td style="padding:4px 0;color:#cbd5e1;">Save filter — persisted in browser storage across sessions</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;">✕</td><td style="padding:4px 0;color:#cbd5e1;">Clear active filter</td></tr>
      </table>
    </div>

    <!-- General -->
    <div>
      <div style="font-size:11px;font-weight:700;text-transform:uppercase;letter-spacing:.07em;color:#4a5568;margin-bottom:10px;">General</div>
      <table style="border-collapse:collapse;width:100%;font-size:13px;">
        <tr><td style="padding:4px 0;width:56%;color:#a0aec0;">Auto-reload</td><td style="padding:4px 0;color:#cbd5e1;">Page reloads automatically when GGV regenerates the SVG</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:1px 7px;font-size:11px;">?</kbd></td><td style="padding:4px 0;color:#cbd5e1;">Toggle this help overlay</td></tr>
        <tr><td style="padding:4px 0;color:#a0aec0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:1px 7px;font-size:11px;">Esc</kbd></td><td style="padding:4px 0;color:#cbd5e1;">Close this help overlay</td></tr>
      </table>
    </div>
  </div>
</div>
<script>
(function(){{
  var inp = document.getElementById('ggv-flt-inp');
  var ind = document.getElementById('ggv-flt-ind');
  function refresh() {{
    var v = localStorage.getItem('ggv-diff-filter') || '';
    inp.value = v;
    ind.style.display = v ? 'inline' : 'none';
    ind.title = v ? 'Active filter: ' + v : '';
  }}
  refresh();
  document.addEventListener('visibilitychange', function() {{ if (!document.hidden) refresh(); }});
  window.addEventListener('focus', refresh);
  inp.addEventListener('keydown', function(e){{ if(e.key==='Enter') ggvSave(); }});
  window.ggvSave = function(){{
    var v = inp.value.trim();
    if(v) localStorage.setItem('ggv-diff-filter', v);
    else localStorage.removeItem('ggv-diff-filter');
    refresh();
  }};
  window.ggvClear = function(){{
    localStorage.removeItem('ggv-diff-filter');
    inp.value = '';
    refresh();
  }};
  var overlay = document.getElementById('ggv-help-overlay');
  window.ggvHelp = function(){{
    var visible = overlay.style.display === 'flex';
    overlay.style.display = visible ? 'none' : 'flex';
  }};
  document.addEventListener('keydown', function(e){{
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA') return;
    if (e.key === '?') {{ ggvHelp(); return; }}
    if (e.key === 'Escape') {{ overlay.style.display = 'none'; }}
  }});
}})();
</script>
</body>
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

fn run_tag_delete(repo_path: &str, name: &str) {
    // Delete locally
    let _ = std::process::Command::new("git")
        .args(["-C", repo_path, "tag", "-d", name])
        .status();
    // Delete from all remotes
    let remotes_out = std::process::Command::new("git")
        .args(["-C", repo_path, "remote"])
        .output();
    if let Ok(out) = remotes_out {
        let remotes = String::from_utf8_lossy(&out.stdout);
        for remote in remotes.lines() {
            let _ = std::process::Command::new("git")
                .args(["-C", repo_path, "push", remote, "--delete", name])
                .status();
        }
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

/// Splits a filter string (e.g. "*.cpp *.h") into validated pathspec tokens.
/// Allows: alphanumeric, `*`, `?`, `.`, `/`, `-`, `_`, `[`, `]`.
fn parse_pathspec(filter: &str) -> Vec<String> {
    filter
        .split_whitespace()
        .filter(|s| {
            !s.is_empty()
                && s.len() <= 200
                && s.chars().all(|c| {
                    c.is_alphanumeric()
                        || matches!(c, '*' | '?' | '.' | '/' | '-' | '_' | '[' | ']')
                })
        })
        .map(|s| s.to_string())
        .collect()
}

fn git_log_metadata(
    repo_path: &str,
    base: &str,
    sha1: &str,
    sha2: &str,
) -> std::io::Result<std::process::Output> {
    let exclude_base = format!("^{}", base);
    std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            GIT_LOG_METADATA_FORMAT,
            "--name-status",
            &exclude_base,
            sha1,
            sha2,
        ])
        .output()
}

/// Resolves the best human-readable label for a commit SHA.
/// Prefers branch/tag decorations from `git log -1 --pretty=format:%D`.
fn get_ref_label(repo_path: &str, sha: &str) -> String {
    let short = &sha[..sha.len().min(7)];
    if let Ok(out) = std::process::Command::new("git")
        .args(["-C", repo_path, "log", "-1", "--pretty=format:%D", sha])
        .output()
    {
        let deco = String::from_utf8_lossy(&out.stdout);
        let deco = deco.trim();
        if !deco.is_empty() {
            for part in deco.split(',').map(str::trim) {
                if let Some(branch) = part.strip_prefix("HEAD -> ") {
                    return format!("{branch} ({short})");
                }
                if !part.starts_with("HEAD") && !part.is_empty() {
                    return format!("{part} ({short})");
                }
            }
        }
    }
    short.to_string()
}

/// Writes a temp file instructing the AI to use a specific heading.
/// Returns the file path on success.
fn write_header_file(label1: &str, label2: &str) -> Option<std::path::PathBuf> {
    let path = std::env::temp_dir().join("ggv_header.txt");
    let content =
        format!("Use the following as the heading/title of your response: \"{label1} → {label2}\"");
    std::fs::write(&path, content.as_bytes()).ok()?;
    Some(path)
}

fn has_git_diff(repo_path: &str, sha1: &str, sha2: &str, pathspecs: &[String]) -> bool {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-C", repo_path, "diff", "--quiet", sha1, sha2]);
    if !pathspecs.is_empty() {
        cmd.arg("--");
        cmd.args(pathspecs);
    }
    cmd.status().map(|s| !s.success()).unwrap_or(true)
}

fn build_no_diff_html(sha1: &str, sha2: &str, theme: Theme) -> String {
    let s1 = &sha1[..sha1.len().min(7)];
    let s2 = &sha2[..sha2.len().min(7)];
    let (bg, box_bg, box_border, text, sub) = match theme {
        Theme::Dark => ("#0f1117", "#1a1f2e", "#2d3748", "#e2e8f0", "#718096"),
        Theme::Light => ("#f8fafc", "#ffffff", "#e2e8f0", "#1e293b", "#64748b"),
    };
    format!(
        r#"<html><head><meta charset="utf-8"><style>
body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;display:flex;align-items:center;justify-content:center;height:100vh;margin:0;background:{bg}}}
.box{{background:{box_bg};border:1px solid {box_border};border-radius:8px;padding:24px 32px;text-align:center;box-shadow:0 2px 12px rgba(0,0,0,.15)}}
h3{{margin:0 0 8px;color:{text}}}p{{margin:0 0 16px;color:{sub}}}button{{padding:6px 20px;cursor:pointer;border-radius:4px;border:1px solid {box_border};background:{box_bg};color:{text}}}
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

const DIFF2HTML_JS: &str = include_str!("../assets/diff2html.min.js");
const DIFF2HTML_CSS: &str = include_str!("../assets/diff2html.min.css");

/// Find the first child commit of `commit` (i.e. newer by one step).
/// Tries the ancestry path to HEAD first; falls back to scanning all refs.
fn find_child_commit(repo_path: &str, commit: &str) -> Option<String> {
    // Fast path: ancestry path to HEAD
    let out = std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "rev-list",
            "--ancestry-path",
            &format!("{commit}..HEAD"),
        ])
        .output()
        .ok()?;
    if out.status.success() {
        let s = String::from_utf8_lossy(&out.stdout);
        if let Some(h) = s.trim().lines().last() {
            if !h.is_empty() {
                return Some(h.to_string());
            }
        }
    }
    // Fallback: scan all refs for a commit whose parent list contains our hash
    let out = std::process::Command::new("git")
        .args(["-C", repo_path, "log", "--all", "--format=%H %P"])
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines() {
        let mut parts = line.split_whitespace();
        let child_hash = parts.next()?;
        for parent in parts {
            if parent.starts_with(commit) || commit.starts_with(parent) {
                return Some(child_hash.to_string());
            }
        }
    }
    None
}

/// Inject keyboard navigation (`[` = older, `]` = newer) and a floating nav bar
/// into a diff2html-single HTML page just before `</body>`.
fn inject_commit_navigation(html: &str, older: Option<&str>, newer: Option<&str>) -> String {
    let older_js = older.map_or("null".to_string(), |h| format!("'{h}'"));
    let newer_js = newer.map_or("null".to_string(), |h| format!("'{h}'"));

    fn nav_btn(label: &str, href: Option<&str>, title: &str) -> String {
        match href {
            Some(url) => format!(
                r#"<a href="{url}" title="{title}" style="display:inline-flex;align-items:center;gap:4px;padding:5px 10px;background:rgba(30,36,54,0.92);color:#a0aec0;border:1px solid #2d3748;border-radius:6px;font-family:monospace;font-size:12px;text-decoration:none;cursor:pointer;transition:border-color .15s;" onmouseover="this.style.borderColor='#63b3ed'" onmouseout="this.style.borderColor='#2d3748'">{label}</a>"#
            ),
            None => format!(
                r#"<span style="display:inline-flex;align-items:center;gap:4px;padding:5px 10px;background:rgba(30,36,54,0.5);color:#4a5568;border:1px solid #1e2a3a;border-radius:6px;font-family:monospace;font-size:12px;">{label}</span>"#
            ),
        }
    }

    let older_url = older.map(|h| format!("/diff2html-single?commit={h}"));
    let newer_url = newer.map(|h| format!("/diff2html-single?commit={h}"));
    let btn_older = nav_btn("[ ← older", older_url.as_deref(), "Older commit  [");
    let btn_newer = nav_btn("newer → ]", newer_url.as_deref(), "Newer commit  ]");

    let injection = format!(
        r#"<div style="position:fixed;bottom:16px;right:16px;display:flex;gap:6px;z-index:9999;">
  {btn_older}{btn_newer}
  <button onclick="ggvToggleHelp()" title="Keyboard shortcuts  ?" style="display:inline-flex;align-items:center;padding:5px 10px;background:rgba(30,36,54,0.92);color:#a0aec0;border:1px solid #2d3748;border-radius:6px;font-family:monospace;font-size:12px;cursor:pointer;" onmouseover="this.style.borderColor='#63b3ed'" onmouseout="this.style.borderColor='#2d3748'">?</button>
</div>
<div id="ggv-help-overlay" style="display:none;position:fixed;inset:0;background:rgba(0,0,0,0.6);z-index:10000;align-items:center;justify-content:center;">
  <div style="background:#1a1f2e;border:1px solid #2d3748;border-radius:12px;padding:28px 32px;min-width:320px;font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;box-shadow:0 16px 48px rgba(0,0,0,.7);">
    <div style="display:flex;align-items:center;justify-content:space-between;margin-bottom:20px;">
      <span style="color:#e2e8f0;font-size:15px;font-weight:600;">Keyboard Shortcuts</span>
      <button onclick="ggvToggleHelp()" style="background:none;border:none;color:#718096;font-size:18px;cursor:pointer;line-height:1;">&#x2715;</button>
    </div>
    <table style="border-collapse:collapse;width:100%;color:#e2e8f0;font-size:13px;">
      <tr><td style="padding:6px 0;color:#718096;font-size:11px;font-weight:600;text-transform:uppercase;letter-spacing:.05em;" colspan="2">Navigation</td></tr>
      <tr>
        <td style="padding:5px 0;width:80px;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:2px 8px;font-family:monospace;font-size:12px;">[</kbd></td>
        <td style="padding:5px 0;color:#cbd5e1;">Older commit (parent)</td>
      </tr>
      <tr>
        <td style="padding:5px 0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:2px 8px;font-family:monospace;font-size:12px;">]</kbd></td>
        <td style="padding:5px 0;color:#cbd5e1;">Newer commit (child)</td>
      </tr>
      <tr><td style="padding:10px 0 6px;color:#718096;font-size:11px;font-weight:600;text-transform:uppercase;letter-spacing:.05em;" colspan="2">Files</td></tr>
      <tr>
        <td style="padding:5px 0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:2px 8px;font-family:monospace;font-size:12px;">Enter</kbd></td>
        <td style="padding:5px 0;color:#cbd5e1;">Apply file filter</td>
      </tr>
      <tr><td style="padding:10px 0 6px;color:#718096;font-size:11px;font-weight:600;text-transform:uppercase;letter-spacing:.05em;" colspan="2">General</td></tr>
      <tr>
        <td style="padding:5px 0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:2px 8px;font-family:monospace;font-size:12px;">?</kbd></td>
        <td style="padding:5px 0;color:#cbd5e1;">Toggle this help</td>
      </tr>
      <tr>
        <td style="padding:5px 0;"><kbd style="background:#2d3748;color:#a0aec0;border:1px solid #4a5568;border-radius:4px;padding:2px 8px;font-family:monospace;font-size:12px;">Esc</kbd></td>
        <td style="padding:5px 0;color:#cbd5e1;">Close this help</td>
      </tr>
    </table>
  </div>
</div>
<script>
(function(){{
  var older = {older_js};
  var newer = {newer_js};
  var overlay = document.getElementById('ggv-help-overlay');
  window.ggvToggleHelp = function() {{
    var visible = overlay.style.display === 'flex';
    overlay.style.display = visible ? 'none' : 'flex';
  }};
  overlay.addEventListener('click', function(e) {{
    if (e.target === overlay) overlay.style.display = 'none';
  }});
  document.addEventListener('keydown', function(e) {{
    if (e.target.tagName === 'INPUT' || e.target.tagName === 'TEXTAREA') return;
    if (e.key === '?') {{ ggvToggleHelp(); return; }}
    if (e.key === 'Escape') {{ overlay.style.display = 'none'; return; }}
    if (overlay.style.display === 'flex') return;
    if (e.key === '[' && older) {{ window.location.href = '/diff2html-single?commit=' + older; }}
    if (e.key === ']' && newer) {{ window.location.href = '/diff2html-single?commit=' + newer; }}
  }});
}})();
</script>
</body>"#
    );

    html.replacen("</body>", &injection, 1)
}

/// Encode an arbitrary string as a JSON string literal (including surrounding quotes).
/// Also escapes `</` as `<\/` so that `</script>` inside the value cannot terminate
/// an enclosing HTML `<script>` tag.
fn to_json_string(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            '<' => {
                // Escape </ to prevent </script> from closing the enclosing script tag
                if chars.peek() == Some(&'/') {
                    out.push_str("<\\/");
                    chars.next();
                } else {
                    out.push('<');
                }
            }
            c if (c as u32) < 0x20 => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// Returns a self-contained HTML fragment (styles + divs + scripts) showing the commit history
/// cards and side-by-side diff. Designed to be appended after the AI summary card.
fn diff2html_section(
    repo_path: &str,
    sha1: &str,
    sha2: &str,
    theme: Theme,
    pathspecs: &[String],
    filter_str: &str,
    max_diff_files: usize,
) -> Result<String, String> {
    let sha1_is_ancestor = std::process::Command::new("git")
        .args(["-C", repo_path, "merge-base", "--is-ancestor", sha1, sha2])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let (older, newer) = if sha1_is_ancestor {
        (sha1, sha2)
    } else {
        (sha2, sha1)
    };

    // Count changed files before fetching the full diff
    if max_diff_files > 0 {
        let mut name_cmd = std::process::Command::new("git");
        name_cmd.args(["-C", repo_path, "diff", "--name-only", "-w", older, newer]);
        if !pathspecs.is_empty() {
            name_cmd.arg("--");
            name_cmd.args(pathspecs);
        }
        let file_count = name_cmd
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
            .unwrap_or(0);
        if file_count > max_diff_files {
            return Err(format!(
                "Too many changed files ({file_count} > {max_diff_files}); diff suppressed"
            ));
        }
    }

    let mut diff_cmd = std::process::Command::new("git");
    diff_cmd.args(["-C", repo_path, "diff", "-w", older, newer]);
    if !pathspecs.is_empty() {
        diff_cmd.arg("--");
        diff_cmd.args(pathspecs);
    }
    let diff_bytes = diff_cmd
        .output()
        .map_err(|e| format!("git diff failed: {e}"))?
        .stdout;

    // Use the merge base as the log exclusion anchor so that commits from both
    // sides of a diverging branch pair are included, not just one side.
    let merge_base = std::process::Command::new("git")
        .args(["-C", repo_path, "merge-base", sha1, sha2])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let exclude_base = format!("^{}", merge_base.as_deref().unwrap_or(older));
    let log_range = [exclude_base.as_str(), sha1, sha2];
    let log_text = std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            "--pretty=format:### %h, %an, %ar, %D%n%n%s%n%b%n",
        ])
        .args(log_range)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let file_counts = batch_file_counts(repo_path, &log_range);
    let (commit_cards, commit_count) = render_commit_cards(&file_counts, &log_text);
    let count_label = if commit_count == 1 {
        "1 commit".to_string()
    } else {
        format!("{commit_count} commits")
    };

    let raw_diff = String::from_utf8_lossy(&diff_bytes);
    let s1 = &sha1[..sha1.len().min(7)];
    let s2 = &sha2[..sha2.len().min(7)];
    let diff_json = to_json_string(&raw_diff);
    let filter_json = to_json_string(filter_str);

    let (
        bg,
        card_bg,
        card_border,
        card_hover,
        text,
        sub,
        dim,
        h1_col,
        sha_bg,
        sha_fg,
        hash_bg,
        hash_fg,
        rh_bg,
        rh_fg,
        rb_bg,
        rb_fg,
        rr_bg,
        rr_fg,
        rt_bg,
        rt_fg,
        section_border,
    ) = match theme {
        Theme::Dark => (
            "#0f1117", "#1a1f2e", "#2d3748", "#4a5568", "#e2e8f0", "#718096", "#4a5568", "#63b3ed",
            "#2d3748", "#a0aec0", "#1e3a5f", "#63b3ed", "#744210", "#fbd38d", "#1e3a5f", "#63b3ed",
            "#1c4532", "#68d391", "#521b41", "#fbb6ce", "#2d3748",
        ),
        Theme::Light => (
            "#f8fafc", "#ffffff", "#e2e8f0", "#cbd5e1", "#1e293b", "#475569", "#94a3b8", "#2563eb",
            "#f1f5f9", "#64748b", "#eff6ff", "#1d4ed8", "#fef3c7", "#92400e", "#eff6ff", "#1e40af",
            "#ecfdf5", "#065f46", "#fdf4ff", "#7e22ce", "#e2e8f0",
        ),
    };

    Ok(format!(
        r#"<style>
/* ── File filter bar ── */
.ggv-filter-bar {{
  display: flex; align-items: center; gap: 8px; flex-wrap: wrap;
  padding: 8px 16px; background: {bg}; border-bottom: 1px solid {section_border};
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  font-size: 13px; margin-top: 32px; border-top: 2px solid {section_border};
}}
.ggv-flt-label {{ color: {text}; white-space: nowrap; }}
.ggv-flt-input {{
  flex: 1; max-width: 360px; padding: 4px 8px;
  border: 1px solid {section_border}; border-radius: 4px;
  background: {card_bg}; color: {text};
  font-family: monospace; font-size: 12px;
}}
.ggv-flt-btn {{
  padding: 4px 12px; border-radius: 4px; cursor: pointer;
  border: 1px solid {section_border}; background: {card_bg}; color: {text};
  font-size: 12px;
}}
.ggv-flt-btn:hover {{ background: {card_border}; }}
.ggv-flt-active {{ color: #f6ad55; font-size: 11px; white-space: nowrap; }}
/* ── Commit history section ── */
.ggv-history {{
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  background: {bg}; color: {text};
  padding: 24px 16px 0;
}}
.ggv-history .page {{ max-width: 1200px; margin: 0 auto; padding-bottom: 24px; border-bottom: 2px solid {section_border}; }}
.ggv-history .hdr {{
  display: flex; align-items: center; gap: 10px;
  margin-bottom: 16px; flex-wrap: wrap;
}}
.ggv-history .hdr h1 {{ font-size: 15px; color: {h1_col}; font-weight: 600; }}
.ggv-history .sha {{
  font-family: monospace; font-size: 12px;
  background: {sha_bg}; padding: 2px 8px;
  border-radius: 4px; color: {sha_fg};
}}
.ggv-history .arrow {{ color: {dim}; }}
.ggv-history .count {{ font-size: 12px; color: {dim}; margin-left: auto; }}
.ggv-history .commit {{
  background: {card_bg}; border: 1px solid {card_border};
  border-radius: 8px; padding: 12px 16px; margin-bottom: 6px;
}}
.ggv-history .commit:hover {{ border-color: {card_hover}; }}
.ggv-history .commit.compare-first {{ border-color: #f6ad55 !important; box-shadow: 0 0 0 2px rgba(246,173,85,0.2); }}
.ggv-history .meta {{
  display: flex; align-items: center; gap: 6px;
  margin-bottom: 5px; flex-wrap: wrap;
}}
.ggv-history .hash {{
  font-family: monospace; font-size: 12px;
  background: {hash_bg}; color: {hash_fg};
  padding: 1px 6px; border-radius: 4px; font-weight: 600;
  text-decoration: none; cursor: pointer;
}}
.ggv-history .hash:hover {{ opacity: 0.8; text-decoration: underline; }}
.ggv-history .author {{ font-size: 12px; color: {sub}; }}
.ggv-history .time {{ font-size: 12px; color: {dim}; margin-left: auto; }}
.ggv-history .subject {{ font-size: 13px; font-weight: 500; color: {text}; }}
.ggv-history .body {{
  font-size: 12px; color: {sub}; white-space: pre-wrap;
  margin-top: 5px; line-height: 1.5;
}}
.ggv-history .ref-head {{
  background: {rh_bg}; color: {rh_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px; font-weight: 700;
}}
.ggv-history .ref-branch {{
  background: {rb_bg}; color: {rb_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.ggv-history .ref-remote {{
  background: {rr_bg}; color: {rr_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.ggv-history .ref-tag {{
  background: {rt_bg}; color: {rt_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.ggv-history .empty {{ color: {dim}; font-size: 13px; padding: 20px 0; }}
.ggv-history .file-count {{ font-size: 11px; color: {dim}; white-space: nowrap; }}
/* ── diff2html section ── */
{css}
.ggv-diff {{ padding: 16px; max-width: 1200px; margin: 0 auto; background: #fff; color: #24292e; }}
.d2h-file-header {{ cursor: pointer; user-select: none; }}
.d2h-file-header:hover {{ background: #e8eaed; }}
.ggv-toggle {{ float: right; font-size: 11px; color: #888; margin-left: 8px; transition: transform 0.15s; display: inline-block; }}
.ggv-collapsed .ggv-toggle {{ transform: rotate(-90deg); }}
.ggv-file-body {{ overflow: hidden; }}
.ggv-file-body.ggv-collapsed {{ display: none; }}
</style>
<div class="ggv-filter-bar">
  <span class="ggv-flt-label">File filter:</span>
  <input id="ggv-flt" class="ggv-flt-input" type="text" placeholder="*.cpp *.h  or  src/ *.cs">
  <button class="ggv-flt-btn" onclick="ggvApplyFilter()">Apply</button>
  <button class="ggv-flt-btn" onclick="ggvClearFilter()">Clear</button>
</div>
<div class="ggv-history">
  <div class="page">
    <div class="hdr">
      <h1>Commit History</h1>
      <span class="sha">{s1}</span>
      <span class="arrow">&#8594;</span>
      <span class="sha">{s2}</span>
      <span class="count">{count_label}</span>
    </div>
    {commit_cards}
  </div>
</div>
<div class="ggv-diff">
<div id="ggv-diff-content"></div>
</div>
<script>{js}</script>
<script>
document.getElementById('ggv-diff-content').innerHTML =
  Diff2Html.html({diff_json}, {{
    drawFileList: true,
    matching: 'lines',
    outputFormat: 'side-by-side'
  }});
document.querySelectorAll('.d2h-file-wrapper').forEach(function(wrapper) {{
  var header = wrapper.querySelector('.d2h-file-header');
  var body = header && header.nextElementSibling;
  if (!header || !body) return;
  body.classList.add('ggv-file-body');
  var arrow = document.createElement('span');
  arrow.className = 'ggv-toggle';
  arrow.textContent = '\u25bc';
  header.appendChild(arrow);
  header.addEventListener('click', function() {{
    var collapsed = body.classList.toggle('ggv-collapsed');
    header.classList.toggle('ggv-collapsed', collapsed);
  }});
}});
(function(){{
  var inp = document.getElementById('ggv-flt');
  var active = {filter_json};
  inp.value = active || localStorage.getItem('ggv-diff-filter') || '';
  inp.addEventListener('keydown', function(e){{ if(e.key==='Enter') ggvApplyFilter(); }});
}})();
function ggvApplyFilter(){{
  var v = document.getElementById('ggv-flt').value.trim();
  if (v) localStorage.setItem('ggv-diff-filter', v);
  else localStorage.removeItem('ggv-diff-filter');
  var u = new URL(window.location.href);
  if(v){{ u.searchParams.set('filter',v); }} else {{ u.searchParams.delete('filter'); }}
  window.location.href = u.toString();
}}
function ggvClearFilter(){{
  localStorage.removeItem('ggv-diff-filter');
  var u = new URL(window.location.href);
  u.searchParams.delete('filter');
  window.location.href = u.toString();
}}
(function(){{
  var KEY = 'ggv-compare-first';
  var firstSha = localStorage.getItem(KEY);
  var ctxMenu = null;
  function removeMenu() {{ if (ctxMenu) {{ ctxMenu.remove(); ctxMenu = null; }} }}
  function makeItem(label, action) {{
    var item = document.createElement('div');
    item.textContent = label;
    item.style.cssText = 'padding:8px 16px;cursor:pointer;color:#e2e8f0;font-size:13px;white-space:nowrap;';
    item.addEventListener('mouseenter', function() {{ item.style.background = '#2d3748'; }});
    item.addEventListener('mouseleave', function() {{ item.style.background = ''; }});
    item.addEventListener('click', function(e) {{ e.stopPropagation(); removeMenu(); action(); }});
    return item;
  }}
  function makeDivider() {{
    var d = document.createElement('div');
    d.style.cssText = 'height:1px;background:#2d3748;margin:4px 0;';
    return d;
  }}
  function applyHighlight() {{
    document.querySelectorAll('.commit[data-hash]').forEach(function(el) {{
      el.classList.toggle('compare-first', el.dataset.hash === firstSha);
    }});
  }}
  document.addEventListener('click', removeMenu);
  document.addEventListener('keydown', function(e) {{ if (e.key === 'Escape') removeMenu(); }});
  document.querySelectorAll('.commit[data-hash]').forEach(function(el) {{
    el.addEventListener('contextmenu', function(e) {{
      e.preventDefault();
      removeMenu();
      var sha = el.dataset.hash;
      var short = sha.slice(0, 7);
      ctxMenu = document.createElement('div');
      ctxMenu.style.cssText = 'position:fixed;left:' + e.clientX + 'px;top:' + e.clientY + 'px;background:#1a1f2e;border:1px solid #2d3748;border-radius:8px;padding:4px 0;z-index:9999;min-width:210px;box-shadow:0 8px 24px rgba(0,0,0,.6);font-family:"Segoe UI",sans-serif;';
      if (firstSha && firstSha !== sha) {{
        ctxMenu.appendChild(makeItem('Compare with \u2026' + firstSha.slice(0, 7), function() {{
          window.open(window.location.origin + '/diff2html?from=' + firstSha + '&to=' + sha, '_blank');
        }}));
        ctxMenu.appendChild(makeDivider());
      }}
      if (firstSha === sha) {{
        ctxMenu.appendChild(makeItem('Deselect first commit', function() {{
          firstSha = null;
          localStorage.removeItem(KEY);
          applyHighlight();
        }}));
      }} else {{
        ctxMenu.appendChild(makeItem(firstSha ? 'Change first commit' : 'Select as first commit', function() {{
          firstSha = sha;
          localStorage.setItem(KEY, sha);
          applyHighlight();
        }}));
      }}
      document.body.appendChild(ctxMenu);
      var r = ctxMenu.getBoundingClientRect();
      if (r.right > window.innerWidth) ctxMenu.style.left = (e.clientX - r.width) + 'px';
      if (r.bottom > window.innerHeight) ctxMenu.style.top = (e.clientY - r.height) + 'px';
    }});
  }});
  applyHighlight();
}})();
</script>"#,
        css = DIFF2HTML_CSS,
        js = DIFF2HTML_JS,
        s1 = s1,
        s2 = s2,
        diff_json = diff_json,
        filter_json = filter_json,
        commit_cards = commit_cards,
        count_label = count_label,
    ))
}

#[allow(clippy::too_many_arguments)]
fn run_diff2html(
    repo_path: &str,
    sha1: &str,
    sha2: &str,
    theme: Theme,
    pathspecs: &[String],
    filter_str: &str,
    max_diff_files: usize,
    gitlab_url: Option<&str>,
) -> Result<String, String> {
    // Determine chronological order so we always diff older → newer
    let sha1_is_ancestor = std::process::Command::new("git")
        .args(["-C", repo_path, "merge-base", "--is-ancestor", sha1, sha2])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let (older, newer) = if sha1_is_ancestor {
        (sha1, sha2)
    } else {
        (sha2, sha1)
    };

    // Count changed files; skip the full diff if the limit is exceeded.
    let suppressed_file_count: Option<usize> = if max_diff_files > 0 {
        let mut name_cmd = std::process::Command::new("git");
        name_cmd.args(["-C", repo_path, "diff", "--name-only", "-w", older, newer]);
        if !pathspecs.is_empty() {
            name_cmd.arg("--");
            name_cmd.args(pathspecs);
        }
        let file_count = name_cmd
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).lines().count())
            .unwrap_or(0);
        if file_count > max_diff_files {
            Some(file_count)
        } else {
            None
        }
    } else {
        None
    };

    let diff_bytes = if suppressed_file_count.is_none() {
        let mut diff_cmd = std::process::Command::new("git");
        diff_cmd.args(["-C", repo_path, "diff", "-w", older, newer]);
        if !pathspecs.is_empty() {
            diff_cmd.arg("--");
            diff_cmd.args(pathspecs);
        }
        diff_cmd
            .output()
            .map_err(|e| format!("git diff failed: {e}"))?
            .stdout
    } else {
        vec![]
    };

    // Fetch commit log for the history section.
    // Use the merge base as the exclusion anchor so commits from both sides of a
    // diverging branch pair are included, not just one side.
    let merge_base = std::process::Command::new("git")
        .args(["-C", repo_path, "merge-base", sha1, sha2])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    let exclude_base = format!("^{}", merge_base.as_deref().unwrap_or(older));
    let log_range = [exclude_base.as_str(), sha1, sha2];
    let log_text = std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            "--pretty=format:### %h, %an, %ar, %D%n%n%s%n%b%n",
        ])
        .args(log_range)
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let file_counts = batch_file_counts(repo_path, &log_range);
    let (commit_cards, commit_count) = render_commit_cards(&file_counts, &log_text);
    let count_label = if commit_count == 1 {
        "1 commit".to_string()
    } else {
        format!("{commit_count} commits")
    };

    let raw_diff = String::from_utf8_lossy(&diff_bytes);
    let s1 = &sha1[..sha1.len().min(7)];
    let s2 = &sha2[..sha2.len().min(7)];
    let diff_json = to_json_string(&raw_diff);
    let filter_json = to_json_string(filter_str);
    let show_diff = suppressed_file_count.is_none();

    // Theme palette for the commit history section
    let (
        bg,
        card_bg,
        card_border,
        card_hover,
        text,
        sub,
        dim,
        h1_col,
        sha_bg,
        sha_fg,
        hash_bg,
        hash_fg,
        rh_bg,
        rh_fg,
        rb_bg,
        rb_fg,
        rr_bg,
        rr_fg,
        rt_bg,
        rt_fg,
        section_border,
    ) = match theme {
        Theme::Dark => (
            "#0f1117", "#1a1f2e", "#2d3748", "#4a5568", "#e2e8f0", "#718096", "#4a5568", "#63b3ed",
            "#2d3748", "#a0aec0", "#1e3a5f", "#63b3ed", "#744210", "#fbd38d", "#1e3a5f", "#63b3ed",
            "#1c4532", "#68d391", "#521b41", "#fbb6ce", "#2d3748",
        ),
        Theme::Light => (
            "#f8fafc", "#ffffff", "#e2e8f0", "#cbd5e1", "#1e293b", "#475569", "#94a3b8", "#2563eb",
            "#f1f5f9", "#64748b", "#eff6ff", "#1d4ed8", "#fef3c7", "#92400e", "#eff6ff", "#1e40af",
            "#ecfdf5", "#065f46", "#fdf4ff", "#7e22ce", "#e2e8f0",
        ),
    };

    // Build links for the filter bar: git difftool and optional GitLab compare.
    let difftool_url = format!("/diff?from={sha1}&to={sha2}");
    let gitlab_link = gitlab_url.map(|base| {
        let compare_segment = if base.contains("github.com") {
            "/compare/"
        } else {
            "/-/compare/"
        };
        format!(
            r#"<a class="ggv-flt-link" href="{base}{compare_segment}{sha1}...{sha2}" target="_blank">GitLab</a>"#
        )
    }).unwrap_or_default();

    // Build the filter bar (top) and diff section (below commit list) conditionally.
    // When the file limit is exceeded we skip the diff2html library and the git diff output.
    let (diff_filter_bar, diff_section) = if show_diff {
        let filter_bar = format!(
            r#"<div class="ggv-filter-bar">
  <span class="ggv-flt-label">File filter:</span>
  <input id="ggv-flt" class="ggv-flt-input" type="text" placeholder="*.cpp *.h  or  src/ *.cs">
  <button class="ggv-flt-btn" onclick="ggvApplyFilter()">Apply</button>
  <button class="ggv-flt-btn" onclick="ggvClearFilter()">Clear</button>
  <button class="ggv-flt-link" onclick="fetch('{difftool_url}')">Git Difftool</button>
  {gitlab_link}
</div>"#
        );
        let section = format!(
            r#"<div class="ggv-diff">
<div id="diff"></div>
</div>
<script>{js}</script>
<script>
document.getElementById('diff').innerHTML =
  Diff2Html.html({diff_json}, {{
    drawFileList: true,
    matching: 'lines',
    outputFormat: 'side-by-side'
  }});
document.querySelectorAll('.d2h-file-wrapper').forEach(function(wrapper) {{
  var header = wrapper.querySelector('.d2h-file-header');
  var body = header && header.nextElementSibling;
  if (!header || !body) return;
  body.classList.add('ggv-file-body');
  var arrow = document.createElement('span');
  arrow.className = 'ggv-toggle';
  arrow.textContent = '\u25bc';
  header.appendChild(arrow);
  header.addEventListener('click', function() {{
    var collapsed = body.classList.toggle('ggv-collapsed');
    header.classList.toggle('ggv-collapsed', collapsed);
  }});
}});
(function(){{
  var inp = document.getElementById('ggv-flt');
  var active = {filter_json};
  inp.value = active || localStorage.getItem('ggv-diff-filter') || '';
  inp.addEventListener('keydown', function(e){{ if(e.key==='Enter') ggvApplyFilter(); }});
}})();
function ggvApplyFilter(){{
  var v = document.getElementById('ggv-flt').value.trim();
  if (v) localStorage.setItem('ggv-diff-filter', v);
  else localStorage.removeItem('ggv-diff-filter');
  var u = new URL(window.location.href);
  if(v){{ u.searchParams.set('filter',v); }} else {{ u.searchParams.delete('filter'); }}
  window.location.href = u.toString();
}}
function ggvClearFilter(){{
  localStorage.removeItem('ggv-diff-filter');
  var u = new URL(window.location.href);
  u.searchParams.delete('filter');
  window.location.href = u.toString();
}}
</script>"#,
            js = DIFF2HTML_JS,
            diff_json = diff_json,
            filter_json = filter_json,
        );
        (filter_bar, section)
    } else {
        let file_count = suppressed_file_count.unwrap_or(0);
        let notice = format!(
            r#"<div style="font-family:-apple-system,BlinkMacSystemFont,'Segoe UI',sans-serif;padding:24px 32px;font-size:13px;color:#f6ad55;background:#2d1a00;border:1px solid #744210;border-radius:8px;margin:24px auto;max-width:800px;">
  &#9888; Diff suppressed: <strong>{file_count} files</strong> changed, which exceeds the limit of <strong>{max_diff_files}</strong>.
  Use <code>-M &lt;number&gt;</code> to increase the limit, or <code>-M 0</code> to disable it.
</div>"#
        );
        (String::new(), notice)
    };

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>{s1}..{s2}</title>
<style>
/* ── Commit history section ── */
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
.ggv-history {{
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  background: {bg}; color: {text};
  padding: 24px 16px 0;
}}
.ggv-history .page {{ max-width: 1200px; margin: 0 auto; padding-bottom: 24px; border-bottom: 2px solid {section_border}; }}
.ggv-history .hdr {{
  display: flex; align-items: center; gap: 10px;
  margin-bottom: 16px; flex-wrap: wrap;
}}
.ggv-history .hdr h1 {{ font-size: 15px; color: {h1_col}; font-weight: 600; }}
.ggv-history .sha {{
  font-family: monospace; font-size: 12px;
  background: {sha_bg}; padding: 2px 8px;
  border-radius: 4px; color: {sha_fg};
}}
.ggv-history .arrow {{ color: {dim}; }}
.ggv-history .count {{ font-size: 12px; color: {dim}; margin-left: auto; }}
.ggv-history .commit {{
  background: {card_bg}; border: 1px solid {card_border};
  border-radius: 8px; padding: 12px 16px; margin-bottom: 6px;
}}
.ggv-history .commit:hover {{ border-color: {card_hover}; }}
.ggv-history .commit.compare-first {{ border-color: #f6ad55 !important; box-shadow: 0 0 0 2px rgba(246,173,85,0.2); }}
.ggv-history .meta {{
  display: flex; align-items: center; gap: 6px;
  margin-bottom: 5px; flex-wrap: wrap;
}}
.ggv-history .hash {{
  font-family: monospace; font-size: 12px;
  background: {hash_bg}; color: {hash_fg};
  padding: 1px 6px; border-radius: 4px; font-weight: 600;
  text-decoration: none; cursor: pointer;
}}
.ggv-history .hash:hover {{ opacity: 0.8; text-decoration: underline; }}
.ggv-history .author {{ font-size: 12px; color: {sub}; }}
.ggv-history .time {{ font-size: 12px; color: {dim}; margin-left: auto; }}
.ggv-history .subject {{ font-size: 13px; font-weight: 500; color: {text}; }}
.ggv-history .body {{
  font-size: 12px; color: {sub}; white-space: pre-wrap;
  margin-top: 5px; line-height: 1.5;
}}
.ggv-history .ref-head {{
  background: {rh_bg}; color: {rh_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px; font-weight: 700;
}}
.ggv-history .ref-branch {{
  background: {rb_bg}; color: {rb_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.ggv-history .ref-remote {{
  background: {rr_bg}; color: {rr_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.ggv-history .ref-tag {{
  background: {rt_bg}; color: {rt_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.ggv-history .empty {{ color: {dim}; font-size: 13px; padding: 20px 0; }}
.ggv-history .file-count {{ font-size: 11px; color: {dim}; white-space: nowrap; }}
/* ── diff2html section ── */
{css}
.ggv-diff {{ padding: 16px; max-width: 1200px; margin: 0 auto; }}
.d2h-file-header {{ cursor: pointer; user-select: none; }}
.d2h-file-header:hover {{ background: #e8eaed; }}
.ggv-toggle {{ float: right; font-size: 11px; color: #888; margin-left: 8px; transition: transform 0.15s; display: inline-block; }}
.ggv-collapsed .ggv-toggle {{ transform: rotate(-90deg); }}
.ggv-file-body {{ overflow: hidden; }}
.ggv-file-body.ggv-collapsed {{ display: none; }}
/* ── File filter bar ── */
.ggv-filter-bar {{
  display: flex; align-items: center; gap: 8px; flex-wrap: wrap;
  padding: 8px 16px; background: {bg}; border-bottom: 1px solid {section_border};
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  font-size: 13px; position: sticky; top: 0; z-index: 100;
}}
.ggv-flt-label {{ color: {text}; white-space: nowrap; }}
.ggv-flt-input {{
  flex: 1; max-width: 360px; padding: 4px 8px;
  border: 1px solid {section_border}; border-radius: 4px;
  background: {card_bg}; color: {text};
  font-family: monospace; font-size: 12px;
}}
.ggv-flt-btn {{
  padding: 4px 12px; border-radius: 4px; cursor: pointer;
  border: 1px solid {section_border}; background: {card_bg}; color: {text};
  font-size: 12px;
}}
.ggv-flt-btn:hover {{ background: {card_border}; }}
.ggv-flt-link {{
  padding: 4px 10px; border-radius: 4px; cursor: pointer;
  border: 1px solid {section_border}; background: {card_bg}; color: {text};
  font-size: 12px; text-decoration: none; white-space: nowrap;
}}
.ggv-flt-link:hover {{ background: {card_border}; text-decoration: none; }}
</style>
</head>
<body>
{diff_filter_bar}
<div class="ggv-history">
  <div class="page">
    <div class="hdr">
      <h1>Commit History</h1>
      <span class="sha">{s1}</span>
      <span class="arrow">&#8594;</span>
      <span class="sha">{s2}</span>
      <span class="count">{count_label}</span>
    </div>
    {commit_cards}
  </div>
</div>
{diff_section}
<script>
(function(){{
  var KEY = 'ggv-compare-first';
  var firstSha = localStorage.getItem(KEY);
  var ctxMenu = null;
  function removeMenu() {{ if (ctxMenu) {{ ctxMenu.remove(); ctxMenu = null; }} }}
  function makeItem(label, action) {{
    var item = document.createElement('div');
    item.textContent = label;
    item.style.cssText = 'padding:8px 16px;cursor:pointer;color:#e2e8f0;font-size:13px;white-space:nowrap;';
    item.addEventListener('mouseenter', function() {{ item.style.background = '#2d3748'; }});
    item.addEventListener('mouseleave', function() {{ item.style.background = ''; }});
    item.addEventListener('click', function(e) {{ e.stopPropagation(); removeMenu(); action(); }});
    return item;
  }}
  function makeDivider() {{
    var d = document.createElement('div');
    d.style.cssText = 'height:1px;background:#2d3748;margin:4px 0;';
    return d;
  }}
  function applyHighlight() {{
    document.querySelectorAll('.commit[data-hash]').forEach(function(el) {{
      el.classList.toggle('compare-first', el.dataset.hash === firstSha);
    }});
  }}
  document.addEventListener('click', removeMenu);
  document.addEventListener('keydown', function(e) {{ if (e.key === 'Escape') removeMenu(); }});
  document.querySelectorAll('.commit[data-hash]').forEach(function(el) {{
    el.addEventListener('contextmenu', function(e) {{
      e.preventDefault();
      removeMenu();
      var sha = el.dataset.hash;
      ctxMenu = document.createElement('div');
      ctxMenu.style.cssText = 'position:fixed;left:' + e.clientX + 'px;top:' + e.clientY + 'px;background:#1a1f2e;border:1px solid #2d3748;border-radius:8px;padding:4px 0;z-index:9999;min-width:210px;box-shadow:0 8px 24px rgba(0,0,0,.6);font-family:"Segoe UI",sans-serif;';
      if (firstSha && firstSha !== sha) {{
        ctxMenu.appendChild(makeItem('Compare with \u2026' + firstSha.slice(0, 7), function() {{
          window.open(window.location.origin + '/diff2html?from=' + firstSha + '&to=' + sha, '_blank');
        }}));
        ctxMenu.appendChild(makeDivider());
      }}
      if (firstSha === sha) {{
        ctxMenu.appendChild(makeItem('Deselect first commit', function() {{
          firstSha = null;
          localStorage.removeItem(KEY);
          applyHighlight();
        }}));
      }} else {{
        ctxMenu.appendChild(makeItem(firstSha ? 'Change first commit' : 'Select as first commit', function() {{
          firstSha = sha;
          localStorage.setItem(KEY, sha);
          applyHighlight();
        }}));
      }}
      document.body.appendChild(ctxMenu);
      var r = ctxMenu.getBoundingClientRect();
      if (r.right > window.innerWidth) ctxMenu.style.left = (e.clientX - r.width) + 'px';
      if (r.bottom > window.innerHeight) ctxMenu.style.top = (e.clientY - r.height) + 'px';
    }});
  }});
  applyHighlight();
}})();
</script>
</body>
</html>"#,
        css = DIFF2HTML_CSS,
        s1 = s1,
        s2 = s2,
        diff_filter_bar = diff_filter_bar,
        diff_section = diff_section,
        commit_cards = commit_cards,
        count_label = count_label,
    );

    Ok(html)
}

/// Spawns a background thread that polls for a window titled "Recording..."
/// and brings it to the foreground. Stops after finding it or after 10 seconds.
#[cfg(target_os = "windows")]
fn bring_recording_window_to_foreground() {
    std::thread::spawn(|| {
        use std::os::windows::ffi::OsStrExt;
        extern "system" {
            fn FindWindowW(lp_class_name: *const u16, lp_window_name: *const u16) -> isize;
            fn SetForegroundWindow(h_wnd: isize) -> i32;
            fn ShowWindow(h_wnd: isize, n_cmd_show: i32) -> i32;
        }
        let title: Vec<u16> = std::ffi::OsStr::new("Recording...")
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
        loop {
            if std::time::Instant::now() > deadline {
                break;
            }
            let hwnd = unsafe { FindWindowW(std::ptr::null(), title.as_ptr()) };
            if hwnd != 0 {
                unsafe {
                    ShowWindow(hwnd, 9); // SW_RESTORE
                    SetForegroundWindow(hwnd);
                }
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    });
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

fn run_gia_diff(
    repo_path: &str,
    sha1: &str,
    sha2: &str,
    prompt: Option<&str>,
    include_log: bool,
    gia_audio: bool,
    pathspecs: &[String],
) -> String {
    let base = match resolve_diff_base(repo_path, sha1, sha2) {
        Ok(b) => b,
        Err(e) => return format!("Error resolving diff base: {e}"),
    };

    // Determine chronological order (same logic as run_diff2html) so we
    // always diff older → newer. When sha2 is an ancestor of sha1, using
    // `base` (= sha2) as both start and end produced an empty diff.
    let sha1_is_ancestor = std::process::Command::new("git")
        .args(["-C", repo_path, "merge-base", "--is-ancestor", sha1, sha2])
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    let (older, newer) = if sha1_is_ancestor {
        (sha1, sha2)
    } else {
        (sha2, sha1)
    };

    let mut diff_cmd = std::process::Command::new("git");
    diff_cmd.args(["-C", repo_path, "diff", "-w", older, newer]);
    if !pathspecs.is_empty() {
        diff_cmd.arg("--");
        diff_cmd.args(pathspecs);
    }
    let diff_out = match diff_cmd.output() {
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

    // Commit metadata: log(both sides relative to base) with branch/tag decorations
    let meta_path = std::env::temp_dir().join("ggv_meta.txt");
    let has_meta = if include_log {
        let log_out = match git_log_metadata(repo_path, &base, sha1, sha2) {
            Ok(out) => out,
            Err(e) => return format!("Error running git log: {e}"),
        };
        if !log_out.status.success() {
            let stderr = String::from_utf8_lossy(&log_out.stderr).trim().to_string();
            return format!("git log exited with {}: {}", log_out.status, stderr);
        }
        !log_out.stdout.is_empty() && std::fs::write(&meta_path, &log_out.stdout).is_ok()
    } else {
        false
    };

    let label1 = get_ref_label(repo_path, sha1);
    let label2 = get_ref_label(repo_path, sha2);
    let header_path = write_header_file(&label1, &label2);

    let loaded_prompt = load_diff_prompt();
    let effective_prompt = prompt.unwrap_or(&loaded_prompt);
    let mut gia_args: Vec<String> = vec!["--markdown".to_string(), effective_prompt.to_string()];
    if gia_audio {
        gia_args.push("-a".to_string());
        gia_args.push("--audio-dialog-text".to_string());
        gia_args.push(AUDIO_DIALOG_TEXT.to_string());
    }
    if let Some(ref hp) = header_path {
        gia_args.push("-f".to_string());
        gia_args.push(hp.to_string_lossy().into_owned());
    }
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
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return "The AI summary feature requires **GIA**, a command-line tool that was not found in PATH.\nPlease install it from: [https://github.com/panjamo/gia](https://github.com/panjamo/gia)".to_string();
        }
        Err(e) => return format!("Error starting gia: {e}"),
    };

    #[cfg(target_os = "windows")]
    if gia_audio {
        bring_recording_window_to_foreground();
    }

    if let Some(mut stdin) = gia.stdin.take() {
        let _ = stdin.write_all(&diff);
    }

    let result = match gia.wait_with_output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let text = if stdout.is_empty() {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            } else {
                stdout
            };
            if text.contains("Recording cancelled by user") {
                String::new()
            } else {
                text
            }
        }
        Err(e) => format!("Error waiting for gia: {e}"),
    };

    if let Some(ref hp) = header_path {
        let _ = std::fs::remove_file(hp);
    }
    if has_meta {
        let _ = std::fs::remove_file(&meta_path);
    }

    result
}

fn run_gia_log(
    repo_path: &str,
    sha1: &str,
    sha2: &str,
    prompt: Option<&str>,
    gia_audio: bool,
) -> String {
    let base = match resolve_diff_base(repo_path, sha1, sha2) {
        Ok(b) => b,
        Err(e) => return format!("Error resolving log base: {e}"),
    };

    let log_out = match git_log_metadata(repo_path, &base, sha1, sha2) {
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

    let label1 = get_ref_label(repo_path, sha1);
    let label2 = get_ref_label(repo_path, sha2);
    let header_path = write_header_file(&label1, &label2);

    let effective_prompt = prompt.unwrap_or(DEFAULT_LOG_PROMPT);
    let mut gia_args: Vec<String> = vec!["--markdown".to_string(), effective_prompt.to_string()];
    if gia_audio {
        gia_args.push("-a".to_string());
        gia_args.push("--audio-dialog-text".to_string());
        gia_args.push(AUDIO_DIALOG_TEXT.to_string());
    }
    if let Some(ref hp) = header_path {
        gia_args.push("-f".to_string());
        gia_args.push(hp.to_string_lossy().into_owned());
    }
    let mut gia = match std::process::Command::new("gia")
        .args(&gia_args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return "The AI summary feature requires **GIA**, a command-line tool that was not found in PATH.\nPlease install it from: [https://github.com/panjamo/gia](https://github.com/panjamo/gia)".to_string();
        }
        Err(e) => return format!("Error starting gia: {e}"),
    };

    #[cfg(target_os = "windows")]
    if gia_audio {
        bring_recording_window_to_foreground();
    }

    if let Some(mut stdin) = gia.stdin.take() {
        let _ = stdin.write_all(&log_out.stdout);
    }

    let result = match gia.wait_with_output() {
        Ok(out) => {
            let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
            let text = if stdout.is_empty() {
                String::from_utf8_lossy(&out.stderr).trim().to_string()
            } else {
                stdout
            };
            if text.contains("Recording cancelled by user") {
                String::new()
            } else {
                text
            }
        }
        Err(e) => format!("Error waiting for gia: {e}"),
    };

    if let Some(ref hp) = header_path {
        let _ = std::fs::remove_file(hp);
    }

    result
}

fn serve_git_log(repo_path: &str, sha1: &str, sha2: &str, theme: Theme) -> String {
    let base = match resolve_diff_base(repo_path, sha1, sha2) {
        Ok(b) => b,
        Err(e) => return format!("<pre>Error resolving log base: {}</pre>", html_escape(&e)),
    };

    let exclude_base = format!("^{}", base);
    let out = match std::process::Command::new("git")
        .args([
            "-C",
            repo_path,
            "log",
            "--pretty=format:### %h, %an, %ar, %D%n%n%s%n%b%n",
            &exclude_base,
            sha1,
            sha2,
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

    let text = String::from_utf8_lossy(&out.stdout).to_string();
    build_log_html(
        repo_path,
        &sha1[..sha1.len().min(7)],
        &sha2[..sha2.len().min(7)],
        &text,
        theme,
    )
}

fn render_ref_badge(r: &str) -> String {
    let r = r.trim();
    if r.is_empty() {
        return String::new();
    }
    if let Some(stripped) = r.strip_prefix("HEAD -> ") {
        let branch = html_escape(stripped);
        format!(r#"<span class="ref-head">HEAD</span><span class="ref-branch">{branch}</span>"#)
    } else if let Some(stripped) = r.strip_prefix("tag: ") {
        let tag = html_escape(stripped);
        format!(r#"<span class="ref-tag">{tag}</span>"#)
    } else if r.contains('/') {
        format!(r#"<span class="ref-remote">{}</span>"#, html_escape(r))
    } else {
        format!(r#"<span class="ref-branch">{}</span>"#, html_escape(r))
    }
}

fn render_ref_badges(refs_str: &str) -> String {
    refs_str
        .split(", ")
        .map(render_ref_badge)
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// Parses a git log text (format: `### hash, author, rel_time[, refs]\n\nsubject\nbody`)
/// into styled HTML commit cards. Returns (cards_html, count).
/// Returns a map from short commit hash (as it appears in %h log output) → number of changed files.
/// Uses a single `git log --name-only` call over the given revision range.
fn batch_file_counts(
    repo_path: &str,
    range_args: &[&str],
) -> std::collections::HashMap<String, usize> {
    let mut cmd = std::process::Command::new("git");
    cmd.args(["-C", repo_path, "log", "--format=COMMIT %h", "--name-only"]);
    cmd.args(range_args);
    let out = match cmd.output() {
        Ok(o) => o,
        Err(_) => return Default::default(),
    };
    let text = String::from_utf8_lossy(&out.stdout);
    let mut map = std::collections::HashMap::new();
    let mut current_hash: Option<String> = None;
    let mut file_count = 0usize;
    for line in text.lines() {
        if let Some(hash) = line.strip_prefix("COMMIT ") {
            if let Some(h) = current_hash.take() {
                map.insert(h, file_count);
            }
            current_hash = Some(hash.to_string());
            file_count = 0;
        } else if !line.trim().is_empty() {
            file_count += 1;
        }
    }
    if let Some(h) = current_hash {
        map.insert(h, file_count);
    }
    map
}

fn render_commit_cards(
    file_counts: &std::collections::HashMap<String, usize>,
    log_text: &str,
) -> (String, usize) {
    let stripped = log_text.trim_start_matches("### ");
    let raw_commits: Vec<&str> = stripped.split("\n### ").collect();

    let mut cards = String::new();
    let mut count = 0usize;

    for entry in &raw_commits {
        let mut lines = entry.lines();
        let header = match lines.next() {
            Some(h) if !h.trim().is_empty() => h.trim(),
            _ => continue,
        };

        let mut parts = header.splitn(4, ", ");
        let hash = parts.next().unwrap_or("");
        let author = parts.next().unwrap_or("");
        let rel_time = parts.next().unwrap_or("");
        let refs_str = parts.next().unwrap_or("");

        let mut subject = String::new();
        let mut body_lines: Vec<&str> = Vec::new();
        let mut past_blank = false;
        for line in lines {
            if !past_blank {
                if line.trim().is_empty() {
                    past_blank = true;
                }
                continue;
            }
            if subject.is_empty() {
                subject = line.to_string();
            } else {
                body_lines.push(line);
            }
        }
        while body_lines
            .last()
            .map(|l: &&str| l.trim().is_empty())
            .unwrap_or(false)
        {
            body_lines.pop();
        }

        let ref_badges = render_ref_badges(refs_str);
        let body_html = if body_lines.is_empty() {
            String::new()
        } else {
            format!(
                r#"<div class="body">{}</div>"#,
                html_escape(&body_lines.join("\n"))
            )
        };

        let file_count = file_counts.get(hash).copied().unwrap_or(0);
        let file_badge = if file_count == 1 {
            r#" <span class="file-count">1 file</span>"#.to_string()
        } else {
            format!(r#" <span class="file-count">{file_count} files</span>"#)
        };

        cards.push_str(&format!(
            r#"<div class="commit" data-hash="{hash_raw}">
  <div class="meta">
    <a class="hash" href="/diff2html-single?commit={hash_raw}" title="View this commit">{hash}</a>{refs}<span class="author">{author}</span>
    <span class="time">{time}</span>{file_badge}
  </div>
  <div class="subject">{subject}</div>{body}
</div>"#,
            hash_raw = html_escape(hash),
            hash = html_escape(hash),
            refs = if ref_badges.is_empty() {
                String::new()
            } else {
                format!(" {ref_badges} ")
            },
            author = html_escape(author),
            time = html_escape(rel_time),
            file_badge = file_badge,
            subject = html_escape(&subject),
            body = if body_html.is_empty() {
                String::new()
            } else {
                format!("\n  {body_html}")
            },
        ));
        count += 1;
    }

    if count == 0 {
        cards.push_str(r#"<p class="empty">No commits found in this range.</p>"#);
    }

    (cards, count)
}

fn build_log_html(repo_path: &str, sha1: &str, sha2: &str, log_text: &str, theme: Theme) -> String {
    let file_counts = batch_file_counts(repo_path, &[&format!("^{sha1}"), sha2]);
    let (cards, count) = render_commit_cards(&file_counts, log_text);

    let count_label = if count == 1 {
        "1 commit".to_string()
    } else {
        format!("{count} commits")
    };

    // Palette — (bg, card_bg, card_border, card_hover, text, sub, dim, h1,
    //            sha_bg, sha_fg, hash_bg, hash_fg,
    //            ref_head_bg, ref_head_fg, ref_branch_bg, ref_branch_fg,
    //            ref_remote_bg, ref_remote_fg, ref_tag_bg, ref_tag_fg)
    let (
        bg,
        card_bg,
        card_border,
        card_hover,
        text,
        sub,
        dim,
        h1_col,
        sha_bg,
        sha_fg,
        hash_bg,
        hash_fg,
        rh_bg,
        rh_fg,
        rb_bg,
        rb_fg,
        rr_bg,
        rr_fg,
        rt_bg,
        rt_fg,
    ) = match theme {
        Theme::Dark => (
            "#0f1117", "#1a1f2e", "#2d3748", "#4a5568", "#e2e8f0", "#718096", "#4a5568", "#63b3ed",
            "#2d3748", "#a0aec0", "#1e3a5f", "#63b3ed", "#744210", "#fbd38d", "#1e3a5f", "#63b3ed",
            "#1c4532", "#68d391", "#521b41", "#fbb6ce",
        ),
        Theme::Light => (
            "#f8fafc", "#ffffff", "#e2e8f0", "#cbd5e1", "#1e293b", "#475569", "#94a3b8", "#2563eb",
            "#f1f5f9", "#64748b", "#eff6ff", "#1d4ed8", "#fef3c7", "#92400e", "#eff6ff", "#1e40af",
            "#ecfdf5", "#065f46", "#fdf4ff", "#7e22ce",
        ),
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>Commit History</title>
<style>
* {{ box-sizing: border-box; margin: 0; padding: 0; }}
body {{
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
  background: {bg}; color: {text};
  padding: 32px 16px; min-height: 100vh;
}}
.page {{ max-width: 900px; margin: 0 auto; }}
.hdr {{
  display: flex; align-items: center; gap: 10px;
  margin-bottom: 24px; flex-wrap: wrap;
}}
.hdr h1 {{ font-size: 16px; color: {h1_col}; font-weight: 600; }}
.sha {{
  font-family: monospace; font-size: 12px;
  background: {sha_bg}; padding: 2px 8px;
  border-radius: 4px; color: {sha_fg};
}}
.arrow {{ color: {dim}; }}
.count {{ font-size: 12px; color: {dim}; margin-left: auto; }}
.commit {{
  background: {card_bg}; border: 1px solid {card_border};
  border-radius: 8px; padding: 14px 18px; margin-bottom: 8px;
}}
.commit:hover {{ border-color: {card_hover}; }}
.commit.compare-first {{ border-color: #f6ad55 !important; box-shadow: 0 0 0 2px rgba(246,173,85,0.2); }}
.meta {{
  display: flex; align-items: center; gap: 6px;
  margin-bottom: 6px; flex-wrap: wrap;
}}
.hash {{
  font-family: monospace; font-size: 12px;
  background: {hash_bg}; color: {hash_fg};
  padding: 1px 6px; border-radius: 4px; font-weight: 600;
  text-decoration: none; cursor: pointer;
}}
.hash:hover {{ opacity: 0.8; text-decoration: underline; }}
.author {{ font-size: 12px; color: {sub}; }}
.time {{ font-size: 12px; color: {dim}; margin-left: auto; }}
.subject {{ font-size: 14px; font-weight: 500; color: {text}; }}
.body {{
  font-size: 12px; color: {sub}; white-space: pre-wrap;
  margin-top: 6px; line-height: 1.6;
}}
.ref-head {{
  background: {rh_bg}; color: {rh_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px; font-weight: 700;
}}
.ref-branch {{
  background: {rb_bg}; color: {rb_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.ref-remote {{
  background: {rr_bg}; color: {rr_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.ref-tag {{
  background: {rt_bg}; color: {rt_fg};
  padding: 1px 5px; border-radius: 3px; font-size: 11px;
}}
.empty {{ color: {dim}; font-size: 14px; text-align: center; padding: 40px; }}
.file-count {{ font-size: 11px; color: {dim}; white-space: nowrap; }}
</style>
</head>
<body>
<div class="page">
  <div class="hdr">
    <h1>Commit History</h1>
    <span class="sha">{sha1}</span>
    <span class="arrow">&#8594;</span>
    <span class="sha">{sha2}</span>
    <span class="count">{count_label}</span>
  </div>
  {cards}
</div>
<script>
(function() {{
  var KEY = 'ggv-compare-first';
  var firstSha = localStorage.getItem(KEY);
  var ctxMenu = null;
  function removeMenu() {{ if (ctxMenu) {{ ctxMenu.remove(); ctxMenu = null; }} }}
  function makeItem(label, action) {{
    var item = document.createElement('div');
    item.textContent = label;
    item.style.cssText = 'padding:8px 16px;cursor:pointer;color:#e2e8f0;font-size:13px;white-space:nowrap;';
    item.addEventListener('mouseenter', function() {{ item.style.background = '#2d3748'; }});
    item.addEventListener('mouseleave', function() {{ item.style.background = ''; }});
    item.addEventListener('click', function(e) {{ e.stopPropagation(); removeMenu(); action(); }});
    return item;
  }}
  function makeDivider() {{
    var d = document.createElement('div');
    d.style.cssText = 'height:1px;background:#2d3748;margin:4px 0;';
    return d;
  }}
  function applyHighlight() {{
    document.querySelectorAll('.commit[data-hash]').forEach(function(el) {{
      el.classList.toggle('compare-first', el.dataset.hash === firstSha);
    }});
  }}
  document.addEventListener('click', removeMenu);
  document.addEventListener('keydown', function(e) {{ if (e.key === 'Escape') removeMenu(); }});
  document.querySelectorAll('.commit[data-hash]').forEach(function(el) {{
    el.addEventListener('contextmenu', function(e) {{
      e.preventDefault();
      removeMenu();
      var sha = el.dataset.hash;
      ctxMenu = document.createElement('div');
      ctxMenu.style.cssText = 'position:fixed;left:' + e.clientX + 'px;top:' + e.clientY + 'px;background:#1a1f2e;border:1px solid #2d3748;border-radius:8px;padding:4px 0;z-index:9999;min-width:210px;box-shadow:0 8px 24px rgba(0,0,0,.6);font-family:"Segoe UI",sans-serif;';
      if (firstSha && firstSha !== sha) {{
        ctxMenu.appendChild(makeItem('Compare with \u2026' + firstSha.slice(0, 7), function() {{
          window.open(window.location.origin + '/diff2html?from=' + firstSha + '&to=' + sha, '_blank');
        }}));
        ctxMenu.appendChild(makeDivider());
      }}
      if (firstSha === sha) {{
        ctxMenu.appendChild(makeItem('Deselect first commit', function() {{
          firstSha = null;
          localStorage.removeItem(KEY);
          applyHighlight();
        }}));
      }} else {{
        ctxMenu.appendChild(makeItem(firstSha ? 'Change first commit' : 'Select as first commit', function() {{
          firstSha = sha;
          localStorage.setItem(KEY, sha);
          applyHighlight();
        }}));
      }}
      document.body.appendChild(ctxMenu);
      var r = ctxMenu.getBoundingClientRect();
      if (r.right > window.innerWidth) ctxMenu.style.left = (e.clientX - r.width) + 'px';
      if (r.bottom > window.innerHeight) ctxMenu.style.top = (e.clientY - r.height) + 'px';
    }});
  }});
  applyHighlight();
}})();
</script>
</body>
</html>"#,
        sha1 = sha1,
        sha2 = sha2,
        count_label = count_label,
        cards = cards,
    )
}

fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn markdown_to_html(md: &str) -> String {
    let mut html_out = String::new();
    let parser = MdParser::new_ext(md, Options::all());
    html::push_html(&mut html_out, parser);
    html_out
}

fn build_html(
    sha1: &str,
    sha2: &str,
    summary: &str,
    theme: Theme,
    diff_section: Option<String>,
) -> String {
    let summary_html = markdown_to_html(summary);
    let (
        bg,
        card_bg,
        card_border,
        text,
        sub,
        dim,
        h1_col,
        sha_bg,
        sha_fg,
        code_bg,
        blockquote_border,
    ) = match theme {
        Theme::Dark => (
            "#0f1117", "#1a1f2e", "#2d3748", "#e2e8f0", "#718096", "#4a5568", "#63b3ed", "#2d3748",
            "#a0aec0", "#2d3748", "#4a5568",
        ),
        Theme::Light => (
            "#f8fafc", "#ffffff", "#e2e8f0", "#1e293b", "#475569", "#94a3b8", "#2563eb", "#f1f5f9",
            "#64748b", "#f1f5f9", "#cbd5e1",
        ),
    };
    let diff_html = diff_section.unwrap_or_default();
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
    background: {bg}; color: {text};
    min-height: 100vh;
  }}
  .ai-wrapper {{
    display: flex; justify-content: center;
    align-items: flex-start; padding: 32px 16px;
  }}
  .card {{
    background: {card_bg}; border: 1px solid {card_border};
    border-radius: 12px; padding: 32px 40px;
    max-width: 960px; width: 100%;
    box-shadow: 0 20px 60px rgba(0,0,0,0.1);
  }}
  h1 {{ font-size: 18px; color: {h1_col}; margin-bottom: 16px; }}
  .shas {{
    font-family: monospace; font-size: 12px; color: {sub};
    margin-bottom: 24px; display: flex; gap: 8px; align-items: center;
  }}
  .sha {{ background: {sha_bg}; padding: 2px 8px; border-radius: 4px; color: {sha_fg}; }}
  .arrow {{ color: {dim}; }}
  .summary {{ line-height: 1.7; color: {text}; font-size: 14px; }}
  .summary h1, .summary h2, .summary h3, .summary h4 {{
    color: {h1_col}; margin: 20px 0 8px;
  }}
  .summary h1 {{ font-size: 20px; }}
  .summary h2 {{ font-size: 17px; }}
  .summary h3 {{ font-size: 15px; }}
  .summary p {{ margin: 8px 0; }}
  .summary ul, .summary ol {{ margin: 8px 0 8px 24px; }}
  .summary li {{ margin: 4px 0; }}
  .summary code {{
    font-family: monospace; font-size: 12px;
    background: {code_bg}; padding: 1px 5px; border-radius: 3px;
  }}
  .summary pre {{
    background: {code_bg}; border-radius: 6px; padding: 12px 16px;
    overflow-x: auto; margin: 10px 0;
  }}
  .summary pre code {{ background: none; padding: 0; font-size: 12px; }}
  .summary blockquote {{
    border-left: 3px solid {blockquote_border}; margin: 10px 0;
    padding: 4px 12px; color: {sub};
  }}
  .summary a {{ color: {h1_col}; }}
  .summary hr {{ border: none; border-top: 1px solid {card_border}; margin: 16px 0; }}
</style>
</head>
<body>
<div class="ai-wrapper">
<div class="card">
  <h1>AI Diff Summary</h1>
  <div class="shas">
    <span class="sha">{sha1}</span>
    <span class="arrow">&#8594;</span>
    <span class="sha">{sha2}</span>
  </div>
  <div class="summary">{summary_html}</div>
</div>
</div>
{diff_html}
</body>
</html>"#,
        sha1 = sha1,
        sha2 = sha2,
        summary_html = summary_html,
        diff_html = diff_html,
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
