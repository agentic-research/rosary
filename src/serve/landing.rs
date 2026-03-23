//! Landing page for rsry HTTP server at /.
//!
//! Content negotiation: browser gets HTML, API client gets JSON.
//! HTML is loaded from /app/static/mcp-landing.html (injected by rig's
//! Dockerfile at build time). Falls back to inline HTML if file not found.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

/// Path where rig's Dockerfile injects the landing page HTML.
const STATIC_LANDING_PATH: &str = "/app/static/mcp-landing.html";

/// Serve the landing page at GET /.
pub(crate) async fn handle_landing(headers: axum::http::HeaderMap) -> Response {
    let accept = headers
        .get("accept")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if accept.contains("application/json") {
        let info = serde_json::json!({
            "name": "rosary",
            "version": env!("CARGO_PKG_VERSION"),
            "description": "autonomous work orchestrator",
            "mcp_endpoint": "/mcp",
            "transport": "streamable-http",
            "tools": TOOL_COUNT,
        });
        (
            StatusCode::OK,
            [("content-type", "application/json")],
            serde_json::to_string_pretty(&info).unwrap_or_default(),
        )
            .into_response()
    } else {
        // Try static file first (injected by rig Dockerfile), fall back to inline
        let html = std::fs::read_to_string(STATIC_LANDING_PATH)
            .unwrap_or_else(|_| FALLBACK_HTML.to_string());
        Html(html).into_response()
    }
}

const TOOL_COUNT: usize = 27;

/// Minimal fallback when /app/static/mcp-landing.html doesn't exist
/// (local dev, non-containerized). The real page is managed by rig.
const FALLBACK_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>rosary — autonomous work orchestrator</title>
  <style>
    @import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@400;600&display=swap');
    body { background: #08080E; color: #E0D9C7; font-family: 'JetBrains Mono', monospace; display: flex; align-items: center; justify-content: center; min-height: 100vh; margin: 0; }
    .c { max-width: 480px; text-align: center; padding: 40px 24px; }
    h1 { color: #D4A574; font-size: 1.5rem; margin-bottom: 8px; }
    .sub { color: #95866E; font-size: 0.8rem; letter-spacing: 0.08em; text-transform: uppercase; margin-bottom: 32px; }
    .cmd { background: rgba(8,8,14,0.8); border: 1px solid #242038; padding: 16px; text-align: left; margin-bottom: 24px; }
    .cmd-label { color: #6B5D4E; font-size: 10px; letter-spacing: 0.1em; text-transform: uppercase; margin-bottom: 8px; }
    .cmd-text { color: #88CCCC; font-size: 12px; word-break: break-all; }
    .note { color: #6B5D4E; font-size: 11px; }
    a { color: #95866E; text-decoration: none; border-bottom: 1px dotted #242038; }
    a:hover { color: #CCA8E8; }
  </style>
</head>
<body>
  <div class="c">
    <h1>rosary</h1>
    <div class="sub">autonomous work orchestrator</div>
    <div class="cmd">
      <div class="cmd-label">connect</div>
      <div class="cmd-text">claude mcp add --transport http rosary https://mcp.rosary.bot/mcp</div>
    </div>
    <div class="note">requires mtls client certificate &middot; 27 tools &middot; <a href="https://github.com/agentic-research/rosary">source</a></div>
  </div>
</body>
</html>"##;
