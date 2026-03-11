use anyhow::{Context, Result};
use std::process::Command;

/// Injects interactive JavaScript into an SVG string.
/// `forge_url` enables drag-to-compare and GitLab/GitHub links.
/// `ws_url` enables diff2html/AI summary links (pass `None` for standalone SVG).
pub fn enhance_svg(svg: &str, forge_url: Option<&str>, ws_url: Option<&str>) -> String {
    let forge_url_js = forge_url.map_or("null".to_string(), |u| {
        format!("\"{}\"", u.replace('\\', "\\\\").replace('"', "\\\""))
    });
    let ws_url_js = ws_url.map_or("null".to_string(), |u| {
        format!("\"{}\"", u.replace('\\', "\\\\").replace('"', "\\\""))
    });
    let script = INTERACTIVE_SVG_SCRIPT
        .replace("FORGE_URL_PLACEHOLDER", &forge_url_js)
        .replace("WS_URL_PLACEHOLDER", &ws_url_js);
    if let Some(idx) = svg.rfind("</svg>") {
        format!("{}{}{}", &svg[..idx], script, &svg[idx..])
    } else {
        format!("{}{}", svg, script)
    }
}

const INTERACTIVE_SVG_SCRIPT: &str = r#"<script type="text/javascript">
//<![CDATA[
(function() {
var GGV_FORGE_URL = FORGE_URL_PLACEHOLDER;
var GGV_WS_URL = WS_URL_PLACEHOLDER;
var _body = document.body || document.documentElement;
function showCopiedToast(text) {
  var toast = document.getElementById('ggv-toast');
  if (!toast) {
    toast = document.createElement('div');
    toast.id = 'ggv-toast';
    toast.setAttribute('style', 'position:fixed;bottom:16px;left:50%;transform:translateX(-50%);pointer-events:none;font-family:"Segoe UI",sans-serif;font-size:11px;color:#718096;padding:3px 8px;');
    _body.appendChild(toast);
  }
  toast.textContent = 'Copied to clipboard: ' + text.slice(0, 8) + '\u2026';
  toast.style.transition = 'none';
  toast.style.opacity = '1';
  clearTimeout(toast._timer);
  toast._timer = setTimeout(function() {
    toast.style.transition = 'opacity 0.4s';
    toast.style.opacity = '0';
  }, 1500);
}
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
    showCopiedToast(sha);
  });
}
function ggvFilterParam() { return ''; }
// Click + drag setup
document.querySelectorAll('g.node').forEach(function(g) {
  g.style.cursor = 'pointer';
  g.addEventListener('click', function() { copyHash(g); });
});
// Edge label tooltips
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
// Drag-to-compare (forge)
if (GGV_FORGE_URL) {
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
            var seg = GGV_FORGE_URL.indexOf('github.com') >= 0 ? '/compare/' : '/-/compare/';
            window.open(GGV_FORGE_URL + seg + fromSha + '...' + toSha, '_blank');
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
// Context menu
(function() {
  var ctxMenu = null;
  function removeCtxMenu() { if (ctxMenu) { ctxMenu.remove(); ctxMenu = null; } }
  document.addEventListener('click', removeCtxMenu);
  document.addEventListener('keydown', function(e) { if (e.key === 'Escape') removeCtxMenu(); });
  function makeMenuItem(label, action) {
    var item = document.createElement('div');
    item.textContent = label;
    item.setAttribute('style', 'padding:8px 16px;cursor:pointer;color:#e2e8f0;font-size:13px;white-space:nowrap;');
    item.addEventListener('mouseenter', function() { item.style.background = '#2d3748'; });
    item.addEventListener('mouseleave', function() { item.style.background = ''; });
    item.addEventListener('click', function(ev) { ev.stopPropagation(); removeCtxMenu(); action(); });
    return item;
  }
  function makeDivider() {
    var d = document.createElement('div');
    d.setAttribute('style', 'border-top:1px solid #2d3748;margin:4px 0;');
    return d;
  }
  function parseNodeId(id) {
    var local = [], remote = [], tags = [];
    var lm = id.match(/~L~([^~]*)/); if (lm) local  = lm[1].split(',').filter(Boolean);
    var rm = id.match(/~R~([^~]*)/); if (rm) remote = rm[1].split(',').filter(Boolean);
    var tm = id.match(/~T~([^~]*)/); if (tm) tags   = tm[1].split(',').filter(Boolean);
    return { local: local, remote: remote, tags: tags };
  }
  var pinSha = null, pinEl = null, pinStrokes = [];
  function clearPinHL() {
    if (!pinEl) return;
    pinEl.querySelectorAll('polygon,ellipse,path,rect').forEach(function(s, i) {
      if (pinStrokes[i] !== undefined) s.setAttribute('stroke', pinStrokes[i]);
    });
    pinEl = null; pinStrokes = [];
  }
  function setPinHL(g) {
    clearPinHL(); pinEl = g;
    g.querySelectorAll('polygon,ellipse,path,rect').forEach(function(s) {
      pinStrokes.push(s.getAttribute('stroke') || '');
      s.setAttribute('stroke', '#f59e0b');
    });
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
      ctxMenu.setAttribute('style', 'position:fixed;left:' + e.clientX + 'px;top:' + e.clientY + 'px;background:#1a1f2e;border:1px solid #2d3748;border-radius:8px;padding:4px 0;z-index:9999;min-width:180px;box-shadow:0 8px 24px rgba(0,0,0,0.6);font-family:"Segoe UI",sans-serif;');
      ctxMenu.addEventListener('click', function(ev) { ev.stopPropagation(); });
      ctxMenu.appendChild(makeMenuItem(pinSha && pinSha !== sha ? 'Change First Node' : 'Select as First Node', function() {
        clearPinHL(); pinSha = sha; setPinHL(g);
      }));
      if (pinSha === sha) {
        ctxMenu.appendChild(makeMenuItem('Clear First Node Selection', function() {
          clearPinHL(); pinSha = null;
        }));
      }
      if (pinSha && pinSha !== sha) {
        var pinShort = pinSha.slice(0, 7);
        var myShort = sha.slice(0, 7);
        var range = pinShort + ' \u2194 ' + myShort;
        var myTop = g.getBoundingClientRect().top;
        var pTop = pinEl ? pinEl.getBoundingClientRect().top : 0;
        var fromSha = pTop > myTop ? pinSha : sha;
        var toSha   = pTop > myTop ? sha : pinSha;
        if (GGV_FORGE_URL) {
          var fSeg = GGV_FORGE_URL.indexOf('github.com') >= 0 ? '/compare/' : '/-/compare/';
          ctxMenu.appendChild(makeDivider());
          ctxMenu.appendChild(makeMenuItem('Open Compare on GitLab \u2044 GitHub \u2013 ' + range, function() {
            window.open(GGV_FORGE_URL + fSeg + fromSha + '...' + toSha, '_blank');
          }));
        }
      }
      ctxMenu.appendChild(makeDivider());
      ctxMenu.appendChild(makeMenuItem('Copy Commit SHA', function() {
        navigator.clipboard.writeText(sha).then(function() { showCopiedToast(sha); });
      }));
      refs.local.forEach(function(name) {
        ctxMenu.appendChild(makeMenuItem('Copy Branch Name: ' + name, function() {
          navigator.clipboard.writeText(name).then(function() { showCopiedToast(name); });
        }));
      });
      refs.remote.forEach(function(name) {
        ctxMenu.appendChild(makeMenuItem('Copy Branch Name: ' + name, function() {
          navigator.clipboard.writeText(name).then(function() { showCopiedToast(name); });
        }));
      });
      refs.tags.forEach(function(name) {
        ctxMenu.appendChild(makeMenuItem('Copy Tag Name: ' + name, function() {
          navigator.clipboard.writeText(name).then(function() { showCopiedToast(name); });
        }));
      });
      _body.appendChild(ctxMenu);
    });
  });
  // Edge context menu
  if (GGV_FORGE_URL) {
    document.querySelectorAll('g.edge').forEach(function(g) {
      var title = g.querySelector('title');
      if (!title) return;
      var m = title.textContent.match(/^([0-9a-f]{40})->([0-9a-f]{40})$/);
      if (!m) return;
      var fromSha = m[1], toSha = m[2];
      g.addEventListener('contextmenu', function(e) {
        e.preventDefault(); e.stopPropagation();
        removeCtxMenu();
        ctxMenu = document.createElement('div');
        ctxMenu.setAttribute('style', 'position:fixed;left:' + e.clientX + 'px;top:' + e.clientY + 'px;background:#1a1f2e;border:1px solid #2d3748;border-radius:8px;padding:4px 0;z-index:9999;min-width:200px;box-shadow:0 8px 24px rgba(0,0,0,0.6);font-family:"Segoe UI",sans-serif;');
        ctxMenu.addEventListener('click', function(ev) { ev.stopPropagation(); });
        var fSeg = GGV_FORGE_URL.indexOf('github.com') >= 0 ? '/compare/' : '/-/compare/';
        ctxMenu.appendChild(makeMenuItem('Open Compare on GitLab \u2044 GitHub', function() {
          window.open(GGV_FORGE_URL + fSeg + fromSha + '...' + toSha, '_blank');
        }));
        _body.appendChild(ctxMenu);
      });
    });
  }
})();
})();
//]]>
</script>
"#;

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

    println!("Opened: {}", file_path);
    Ok(())
}
