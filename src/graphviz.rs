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

pub fn generate_svg(dot_path: &str) -> Result<String> {
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
    inject_clipboard_js(&svg_path_str)?;
    println!("Generated SVG file: {}", svg_path_str);
    Ok(svg_path_str)
}

fn inject_clipboard_js(svg_path: &str) -> Result<()> {
    let content = std::fs::read_to_string(svg_path)
        .with_context(|| format!("Failed to read SVG: {}", svg_path))?;

    let script = r#"<script type="text/ecmascript">
function copyHash(el) {
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
// Offset edge count labels away from the edge line
window.addEventListener('load', function() {
  document.querySelectorAll('g.edge text').forEach(function(t) {
    var x = parseFloat(t.getAttribute('x') || 0);
    t.setAttribute('x', x + 10);
  });
});
</script>"#;

    // Inject script after the opening <svg ...> tag
    let modified = if let Some(svg_start) = content.find("<svg ") {
        if let Some(tag_end) = content[svg_start..].find('>') {
            let insert_at = svg_start + tag_end + 1;
            format!("{}\n{}\n{}", &content[..insert_at], script, &content[insert_at..])
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
