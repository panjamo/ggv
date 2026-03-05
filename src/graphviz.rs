use anyhow::{Context, Result};
use std::path::Path;
use std::process::Command;

pub fn find_dot_executable() -> Option<std::path::PathBuf> {
    use std::path::PathBuf;

    if let Ok(env_path) = std::env::var("GRAPHVIZ_DOT") {
        let path = PathBuf::from(&env_path);
        if path.exists() {
            return Some(path);
        }
    }

    #[cfg(target_os = "windows")]
    {
        let candidates = [
            r"C:\Program Files\Graphviz\bin\dot.exe",
            r"C:\Program Files (x86)\Graphviz\bin\dot.exe",
            r"C:\Graphviz\bin\dot.exe",
        ];
        for candidate in &candidates {
            let path = PathBuf::from(candidate);
            if path.exists() {
                return Some(path);
            }
        }
        if let Ok(output) = Command::new("where").arg("dot").output() {
            if output.status.success() {
                if let Ok(s) = std::str::from_utf8(&output.stdout) {
                    for line in s.lines() {
                        let path = PathBuf::from(line.trim());
                        if path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    #[cfg(not(target_os = "windows"))]
    {
        if let Ok(output) = Command::new("which").arg("dot").output() {
            if output.status.success() {
                if let Ok(s) = std::str::from_utf8(&output.stdout) {
                    if let Some(line) = s.lines().next() {
                        let path = PathBuf::from(line.trim());
                        if path.exists() {
                            return Some(path);
                        }
                    }
                }
            }
        }
    }

    None
}

pub fn generate_svg(
    dot_path: &str,
    forge_url: Option<&str>,
    web_server_url: Option<&str>,
) -> Result<String> {
    let dot_file = Path::new(dot_path);
    let svg_path = dot_file.with_extension("svg");

    let dot_exe = find_dot_executable().ok_or_else(|| {
        anyhow::anyhow!(
            "Graphviz (dot.exe) was not found.\n\
             \n\
             To install Graphviz on Windows:\n\
             \n\
             Option 1 – winget (Windows Package Manager):\n\
             \n\
             winget install --id Graphviz.Graphviz\n\
             \n\
             Option 2 – Chocolatey:\n\
             \n\
             choco install graphviz\n\
             \n\
             Option 3 – Manual download:\n\
             \n\
             https://graphviz.org/download/\n\
             \n\
             After installation, open a new terminal so the PATH is updated.\n\
             Alternatively, set the GRAPHVIZ_DOT environment variable to the full\n\
             path of dot.exe, e.g.:\n\
             \n\
             set GRAPHVIZ_DOT=C:\\Program Files\\Graphviz\\bin\\dot.exe"
        )
    })?;

    println!("Using Graphviz: {}", dot_exe.display());

    let output = Command::new(&dot_exe)
        .args(["-Tsvg", dot_path, "-o"])
        .arg(&svg_path)
        .output()
        .with_context(|| format!("Failed to execute: {}", dot_exe.display()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!("Graphviz dot failed: {}", stderr));
    }

    let svg_path_str = svg_path.to_string_lossy().to_string();
    inject_interactive_js(&svg_path_str, forge_url, web_server_url)?;
    println!("Generated SVG file: {}", svg_path_str);
    Ok(svg_path_str)
}

fn inject_interactive_js(
    svg_path: &str,
    forge_url: Option<&str>,
    web_server_url: Option<&str>,
) -> Result<()> {
    let content = std::fs::read_to_string(svg_path)
        .with_context(|| format!("Failed to read SVG: {}", svg_path))?;

    let forge_url_js = match forge_url {
        Some(url) => {
            let escaped = url.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{}\"", escaped)
        }
        None => "null".to_string(),
    };

    // Build the JS web server URL literal: "http://[::1]:PORT" or null
    let ws_url_js = match web_server_url {
        Some(url) => {
            let escaped = url.replace('\\', "\\\\").replace('"', "\\\"");
            format!("\"{}\"", escaped)
        }
        None => "null".to_string(),
    };

    let script_template = r#"<script type="text/ecmascript">
//<![CDATA[
function copyHash(el) {
  if (window._dragJustHappened) { window._dragJustHappened = false; return; }
  var t = el.querySelector('title');
  if (!t) return;
  var sha = t.textContent.trim();
  if (!/^[0-9a-f]{40}$/.test(sha)) return;
  navigator.clipboard.writeText(sha).then(function() {
    el.querySelectorAll('polygon,ellipse,path,rect').forEach(function(s) {
      var orig = s.getAttribute('stroke');
      s.setAttribute('stroke', '#f59e0b');
      setTimeout(function() { s.setAttribute('stroke', orig); }, 500);
    });
  });
}
window.addEventListener('load', function() {
  // Offset edge count labels away from the edge line; set file-list tooltip
  document.querySelectorAll('g.edge').forEach(function(g) {
    var id = g.getAttribute('id') || '';
    var fileList = id.startsWith('files:') ? id.slice(6).split('|').join('\n') : '';
    g.querySelectorAll('text').forEach(function(t) {
      var x = parseFloat(t.getAttribute('x') || 0);
      t.setAttribute('x', x + 10);
      if (/^\d+$/.test(t.textContent.trim())) {
        t.setAttribute('data-ggv-count', '1');
        if (fileList) {
          var titleEl = document.createElementNS('http://www.w3.org/2000/svg', 'title');
          titleEl.textContent = fileList;
          t.appendChild(titleEl);
        }
      }
    });
  });
  // Drag-to-compare: drag one node onto another to open forge compare view
  var forgeUrl = FORGE_URL_PLACEHOLDER;
  if (forgeUrl) {
    document.querySelectorAll('g.node').forEach(function(g) { g.style.cursor = 'grab'; });
    var drag = null;
    var hlTarget = null, hlStrokes = [];
    function clearHL() {
      if (!hlTarget) return;
      hlTarget.querySelectorAll('polygon,ellipse,path,rect').forEach(function(s, i) {
        if (hlStrokes[i] !== undefined) s.setAttribute('stroke', hlStrokes[i]);
      });
      hlTarget = null; hlStrokes = [];
    }
    function setHL(g) {
      clearHL(); hlTarget = g;
      g.querySelectorAll('polygon,ellipse,path,rect').forEach(function(s) {
        hlStrokes.push(s.getAttribute('stroke') || '');
        s.setAttribute('stroke', '#3B82F6');
      });
    }
    function nodeAt(x, y, skip) {
      var els = document.elementsFromPoint(x, y);
      for (var i = 0; i < els.length; i++) {
        var g = els[i].closest && els[i].closest('g.node');
        if (g && g !== skip) return g;
      }
      return null;
    }
    document.querySelectorAll('g.node').forEach(function(g) {
      g.addEventListener('pointerdown', function(e) {
        if (e.button !== 0) return;
        var t = g.querySelector('title');
        if (!t) return;
        var sha = t.textContent.trim();
        if (!/^[0-9a-f]{40}$/.test(sha)) return;
        drag = {sha: sha, el: g, x0: e.clientX, y0: e.clientY, moved: false};
        e.preventDefault();
      });
    });
    document.addEventListener('pointermove', function(e) {
      if (!drag) return;
      var dx = e.clientX - drag.x0, dy = e.clientY - drag.y0;
      if (!drag.moved && Math.sqrt(dx*dx + dy*dy) > 6) {
        drag.moved = true;
        drag.el.style.opacity = '0.5';
        document.documentElement.style.cursor = 'crosshair';
      }
      if (drag.moved) {
        var target = nodeAt(e.clientX, e.clientY, drag.el);
        if (target) setHL(target); else clearHL();
      }
    });
    document.addEventListener('pointerup', function(e) {
      if (!drag) return;
      var wasMoved = drag.moved;
      drag.el.style.opacity = '';
      document.documentElement.style.cursor = '';
      clearHL();
      if (wasMoved) {
        window._dragJustHappened = true;
        var target = nodeAt(e.clientX, e.clientY, drag.el);
        if (target) {
          var t = target.querySelector('title');
          if (t) {
            var tsha = t.textContent.trim();
            if (/^[0-9a-f]{40}$/.test(tsha)) {
              var dragY = drag.el.getBoundingClientRect().top;
              var targetY = target.getBoundingClientRect().top;
              var fromSha = dragY > targetY ? drag.sha : tsha;
              var toSha   = dragY > targetY ? tsha : drag.sha;
              var seg = forgeUrl.indexOf('github.com') >= 0 ? '/compare/' : '/-/compare/';
              window.open(forgeUrl + seg + fromSha + '...' + toSha, '_blank');
            }
          }
        }
      }
      drag = null;
    });
    document.addEventListener('pointercancel', function() {
      if (!drag) return;
      drag.el.style.opacity = '';
      document.documentElement.style.cursor = '';
      clearHL();
      drag = null;
    });
  }
  // Diff server: make edge count labels clickable
  var wsUrl = WS_URL_PLACEHOLDER;
  if (wsUrl) {
    document.querySelectorAll('g.edge').forEach(function(g) {
      var title = g.querySelector('title');
      if (!title) return;
      var m = title.textContent.match(/^([0-9a-f]{40})->([0-9a-f]{40})$/);
      if (!m) return;
      var fromSha = m[1], toSha = m[2];
      g.querySelectorAll('text').forEach(function(t) {
        if (!t.getAttribute('data-ggv-count')) return;
        t.style.cursor = 'pointer';
        t.style.fill = '#60a5fa';
        t.addEventListener('click', function(e) {
          e.stopPropagation();
          e.preventDefault();
          window.open(wsUrl + '/diff?from=' + fromSha + '&to=' + toSha, '_blank');
        });
      });
    });
  }
  // Right-click context menu on nodes (only when web server is active)
  if (wsUrl) {
    var ctxMenu = null;
    function removeCtxMenu() {
      if (ctxMenu) { ctxMenu.remove(); ctxMenu = null; }
    }
    document.addEventListener('click', removeCtxMenu);
    document.addEventListener('keydown', function(e) { if (e.key === 'Escape') removeCtxMenu(); });
    function makeMenuItem(label, action) {
      var item = document.createElement('div');
      item.textContent = label;
      item.style.cssText = 'padding:8px 16px;cursor:pointer;color:#e2e8f0;font-size:13px;white-space:nowrap;';
      item.addEventListener('mouseenter', function() { item.style.background = '#2d3748'; });
      item.addEventListener('mouseleave', function() { item.style.background = ''; });
      item.addEventListener('click', function(ev) { ev.stopPropagation(); removeCtxMenu(); action(); });
      return item;
    }
    function makeDivider() {
      var d = document.createElement('div');
      d.style.cssText = 'border-top:1px solid #2d3748;margin:4px 0;';
      return d;
    }
    // Parse structured node id: sha~L~local1,local2~R~remote1,remote2~T~tag1,tag2
    // ~ is forbidden in git ref names so it is an unambiguous separator.
    function parseNodeId(id) {
      var local = [], remote = [], tags = [];
      var lm = id.match(/~L~([^~]*)/); if (lm) local  = lm[1].split(',').filter(Boolean);
      var rm = id.match(/~R~([^~]*)/); if (rm) remote = rm[1].split(',').filter(Boolean);
      var tm = id.match(/~T~([^~]*)/); if (tm) tags   = tm[1].split(',').filter(Boolean);
      return { local: local, remote: remote, tags: tags };
    }
    var pinSha = null;
    var pinEl = null;
    var pinStrokes = [];
    function setPinHL(g) {
      clearPinHL();
      pinEl = g;
      g.querySelectorAll('polygon,ellipse,path,rect').forEach(function(s) {
        pinStrokes.push(s.getAttribute('stroke') || '');
        s.setAttribute('stroke', '#f59e0b');
      });
    }
    function clearPinHL() {
      if (!pinEl) return;
      pinEl.querySelectorAll('polygon,ellipse,path,rect').forEach(function(s, i) {
        if (pinStrokes[i] !== undefined) s.setAttribute('stroke', pinStrokes[i]);
      });
      pinEl = null; pinStrokes = [];
    }
    document.querySelectorAll('g.node').forEach(function(g) {
      g.addEventListener('contextmenu', function(e) {
        e.preventDefault();
        removeCtxMenu();
        var t = g.querySelector('title');
        if (!t) return;
        var sha = t.textContent.trim();
        if (!/^[0-9a-f]{40}$/.test(sha)) return;
        var refs = parseNodeId(g.getAttribute('id') || '');
        ctxMenu = document.createElement('div');
        ctxMenu.style.cssText = 'position:fixed;left:' + e.clientX + 'px;top:' + e.clientY + 'px;background:#1a1f2e;border:1px solid #2d3748;border-radius:8px;padding:4px 0;z-index:9999;min-width:180px;box-shadow:0 8px 24px rgba(0,0,0,0.6);font-family:"Segoe UI",sans-serif;';
        ctxMenu.addEventListener('click', function(ev) { ev.stopPropagation(); });
        ctxMenu.appendChild(makeMenuItem('Checkout branch', function() {
          fetch(wsUrl + '/checkout?sha=' + sha);
        }));
        ctxMenu.appendChild(makeDivider());
        ctxMenu.appendChild(makeMenuItem('Copy SHA', function() {
          navigator.clipboard.writeText(sha);
        }));
        refs.local.forEach(function(name) {
          ctxMenu.appendChild(makeMenuItem('Copy branch: ' + name, function() {
            navigator.clipboard.writeText(name);
          }));
        });
        refs.remote.forEach(function(name) {
          ctxMenu.appendChild(makeMenuItem('Copy branch: ' + name, function() {
            navigator.clipboard.writeText(name);
          }));
        });
        refs.tags.forEach(function(name) {
          ctxMenu.appendChild(makeMenuItem('Copy tag: ' + name, function() {
            navigator.clipboard.writeText(name);
          }));
        });
        if (refs.local.length > 0 || refs.remote.length > 0) {
          ctxMenu.appendChild(makeDivider());
          refs.local.forEach(function(name) {
            ctxMenu.appendChild(makeMenuItem('Delete local: ' + name, function() {
              if (!confirm('Force-delete local branch "' + name + '"?')) return;
              fetch(wsUrl + '/delete-branch?name=' + encodeURIComponent(name) + '&scope=local');
            }));
          });
          refs.remote.forEach(function(name) {
            ctxMenu.appendChild(makeMenuItem('Delete remote: ' + name, function() {
              if (!confirm('Delete remote branch "' + name + '"?')) return;
              fetch(wsUrl + '/delete-branch?name=' + encodeURIComponent(name) + '&scope=remote');
            }));
          });
        }
        ctxMenu.appendChild(makeDivider());
        if (pinSha && pinSha !== sha) {
          var pinShort = pinSha.slice(0, 7);
          var myTop = g.getBoundingClientRect().top;
          var pTop = pinEl ? pinEl.getBoundingClientRect().top : 0;
          var fromSha2 = pTop > myTop ? pinSha : sha;
          var toSha2   = pTop > myTop ? sha : pinSha;
          ctxMenu.appendChild(makeMenuItem('Compare with ' + pinShort + '\u2026', function() {
            window.open(wsUrl + '/diff?from=' + fromSha2 + '&to=' + toSha2, '_blank');
          }));
        }
        ctxMenu.appendChild(makeMenuItem(pinSha && pinSha !== sha ? 'Change first node' : 'Select as first node', function() {
          clearPinHL();
          pinSha = sha;
          setPinHL(g);
        }));
        if (pinSha === sha) {
          ctxMenu.appendChild(makeMenuItem('Clear selection', function() {
            clearPinHL();
            pinSha = null;
          }));
        }
        document.body.appendChild(ctxMenu);
      });
    });
    // Right-click context menu on edge count labels
    document.querySelectorAll('g.edge').forEach(function(g) {
      var title = g.querySelector('title');
      if (!title) return;
      var m = title.textContent.match(/^([0-9a-f]{40})->([0-9a-f]{40})$/);
      if (!m) return;
      var fromSha = m[1], toSha = m[2];
      g.querySelectorAll('text').forEach(function(t) {
        if (!t.getAttribute('data-ggv-count')) return;
        t.addEventListener('contextmenu', function(e) {
          e.preventDefault();
          e.stopPropagation();
          removeCtxMenu();
          ctxMenu = document.createElement('div');
          ctxMenu.style.cssText = 'position:fixed;left:' + e.clientX + 'px;top:' + e.clientY + 'px;background:#1a1f2e;border:1px solid #2d3748;border-radius:8px;padding:4px 0;z-index:9999;min-width:200px;box-shadow:0 8px 24px rgba(0,0,0,0.6);font-family:"Segoe UI",sans-serif;';
          ctxMenu.addEventListener('click', function(ev) { ev.stopPropagation(); });
          ctxMenu.appendChild(makeMenuItem('AI Summary of Changes', function() {
            window.open(wsUrl + '/diff?from=' + fromSha + '&to=' + toSha + '&ai=1', '_blank');
          }));
          ctxMenu.appendChild(makeMenuItem('AI Summary (log only)', function() {
            window.open(wsUrl + '/log-summary?from=' + fromSha + '&to=' + toSha, '_blank');
          }));
          ctxMenu.appendChild(makeMenuItem('Show Git Log', function() {
            window.open(wsUrl + '/log?from=' + fromSha + '&to=' + toSha, '_blank');
          }));
          document.body.appendChild(ctxMenu);
        });
      });
    });
  }
});
//]]>
</script>"#;

    let script = script_template
        .replace("FORGE_URL_PLACEHOLDER", &forge_url_js)
        .replace("WS_URL_PLACEHOLDER", &ws_url_js);

    // Inject script after the opening <svg ...> tag
    let modified = if let Some(svg_start) = content.find("<svg ") {
        if let Some(tag_end) = content[svg_start..].find('>') {
            let insert_at = svg_start + tag_end + 1;
            format!(
                "{}\n{}\n{}",
                &content[..insert_at],
                script,
                &content[insert_at..]
            )
        } else {
            content
        }
    } else {
        content
    };

    // Add onclick + pointer cursor to every node <g>
    let modified = modified.replace(
        "class=\"node\">",
        "class=\"node\" onclick=\"copyHash(this)\" style=\"cursor:pointer;\">",
    );

    std::fs::write(svg_path, modified)
        .with_context(|| format!("Failed to write SVG: {}", svg_path))?;

    Ok(())
}

pub fn open_file(file_path: &str) -> Result<()> {
    #[cfg(target_os = "windows")]
    {
        Command::new("cmd")
            .args(["/C", "start", "", file_path])
            .output()
            .with_context(|| format!("Failed to open file: {}", file_path))?;
    }

    #[cfg(target_os = "macos")]
    {
        Command::new("open")
            .arg(file_path)
            .output()
            .with_context(|| format!("Failed to open file: {}", file_path))?;
    }

    #[cfg(target_os = "linux")]
    {
        Command::new("xdg-open")
            .arg(file_path)
            .output()
            .with_context(|| format!("Failed to open file: {}", file_path))?;
    }

    println!("Opened file: {}", file_path);
    Ok(())
}
