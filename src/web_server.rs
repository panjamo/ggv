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
- Focus only on the actual code changes
- Mention only: key changes, affected components, possible breaking changes
- No explanations for beginners
- No introduction or filler text
- Write technically and directly, like a PR review comment

Format:
- <component / area>: <short description of change>
";

const DEFAULT_LOG_PROMPT: &str = DEFAULT_DIFF_PROMPT;

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

pub fn start(
    port: u16,
    repo_path: String,
    svg_path: String,
    prompt: Option<String>,
    lang: String,
    gia_audio: bool,
    theme: Theme,
    mut regen: Option<RegenerateConfig>,
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
            listener, &repo_path, &svg_path, prompt, &lang, gia_audio, theme, regen, last_hb,
        )
    });
    Ok((handle, actual_port))
}

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
                    )
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
    prompt: Option<String>,
    lang: &str,
    gia_audio: bool,
    theme: Theme,
    regen: Option<Arc<RegenerateConfig>>,
    last_hb: Arc<AtomicU64>,
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
            let base_prompt = prompt.as_deref().unwrap_or(DEFAULT_DIFF_PROMPT).to_string();
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
            let diff_section =
                diff2html_section(repo_path, &sha1, &sha2, theme, &pathspecs, &filter_str).ok();
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
            let diff_section =
                diff2html_section(repo_path, &sha1, &sha2, theme, &pathspecs, &filter_str).ok();
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
            match run_diff2html(repo_path, &sha1, &sha2, theme, &pathspecs, &filter_str) {
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
                    s.split_whitespace().next().map(|p| p[..p.len().min(40)].to_string())
                });
            let (sha1, sha2) = match parent {
                Some(p) if !p.is_empty() => (p, commit),
                // Root commit (no parent): diff against empty tree
                _ => ("4b825dc642cb6eb9a060e54bf8d69288fbee4904".to_string(), commit),
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
            match run_diff2html(repo_path, &sha1, &sha2, theme, &pathspecs, &filter_str) {
                Ok(html) => send_response(&mut stream, 200, "text/html; charset=utf-8", &html),
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
    let log_text = std::process::Command::new("git")
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
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let (commit_cards, commit_count) = render_commit_cards(&log_text);
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

fn run_diff2html(
    repo_path: &str,
    sha1: &str,
    sha2: &str,
    theme: Theme,
    pathspecs: &[String],
    filter_str: &str,
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
    let log_text = std::process::Command::new("git")
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
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();

    let (commit_cards, commit_count) = render_commit_cards(&log_text);
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
</style>
</head>
<body>
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
</script>
</body>
</html>"#,
        css = DIFF2HTML_CSS,
        js = DIFF2HTML_JS,
        s1 = s1,
        s2 = s2,
        diff_json = diff_json,
        filter_json = filter_json,
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

    let effective_prompt = prompt.unwrap_or(DEFAULT_DIFF_PROMPT);
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
    if r.starts_with("HEAD -> ") {
        let branch = html_escape(&r["HEAD -> ".len()..]);
        format!(r#"<span class="ref-head">HEAD</span><span class="ref-branch">{branch}</span>"#)
    } else if r.starts_with("tag: ") {
        let tag = html_escape(&r["tag: ".len()..]);
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
fn render_commit_cards(log_text: &str) -> (String, usize) {
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

        cards.push_str(&format!(
            r#"<div class="commit">
  <div class="meta">
    <a class="hash" href="/diff2html-single?commit={hash_raw}" title="View this commit">{hash}</a>{refs}<span class="author">{author}</span>
    <span class="time">{time}</span>
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

fn build_log_html(sha1: &str, sha2: &str, log_text: &str, theme: Theme) -> String {
    let (cards, count) = render_commit_cards(log_text);

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
