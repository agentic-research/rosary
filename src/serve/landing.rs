//! Landing page for rsry HTTP server at /.
//!
//! Uses the rosary design language (DESIGN.md): void bg, JetBrains Mono,
//! bead accents. Template from rig/site/templates/service-landing.html.

use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};

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
        Html(LANDING_HTML).into_response()
    }
}

const TOOL_COUNT: usize = 27;

const LANDING_HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>rosary — autonomous work orchestrator</title>
  <meta name="description" content="MCP server for autonomous agent dispatch, bead tracking, and pipeline verification.">
  <style>
    @import url('https://fonts.googleapis.com/css2?family=JetBrains+Mono:wght@300;400;600;700&display=swap');
    :root {
      --void: #08080E; --bg: #0C0C14; --rose: #E8A0B8; --pink: #F0B8D0;
      --lavender: #CCA8E8; --periwinkle: #B8A0D8; --mint: #A0D8C8;
      --sage: #8ECFA0; --teal: #88CCCC; --amber: #D4A574;
      --text: #E0D9C7; --text-2: #B8A98E; --text-3: #95866E; --text-4: #6B5D4E;
      --border: #242038;
    }
    * { margin: 0; padding: 0; box-sizing: border-box; }
    body {
      background: var(--void); color: var(--text);
      font-family: 'JetBrains Mono', monospace;
      min-height: 100vh; display: flex; align-items: center; justify-content: center;
      background-image:
        radial-gradient(ellipse at 30% 20%, rgba(204,168,232,0.04) 0%, transparent 50%),
        radial-gradient(ellipse at 70% 80%, rgba(160,216,200,0.03) 0%, transparent 50%);
    }
    .container { max-width: 560px; width: 100%; padding: 40px 24px; text-align: center; }
    .sigil { margin-bottom: 24px; opacity: 0.6; }
    .sigil circle { fill: var(--amber); }
    h1 { font-size: 1.5rem; color: var(--amber); margin-bottom: 8px; font-weight: 600; }
    .subtitle { font-size: 0.8rem; color: var(--text-3); letter-spacing: 0.08em; text-transform: uppercase; margin-bottom: 32px; }
    .desc { font-size: 13px; color: var(--text-2); line-height: 1.8; margin-bottom: 32px; text-align: left; }
    .connect-box { background: rgba(8,8,14,0.8); border: 1px solid var(--border); padding: 16px; margin-bottom: 24px; text-align: left; }
    .connect-label { font-size: 10px; color: var(--text-4); letter-spacing: 0.1em; text-transform: uppercase; margin-bottom: 8px; }
    .connect-cmd { font-size: 12px; color: var(--teal); line-height: 1.6; word-break: break-all; }
    .tools { text-align: left; margin-bottom: 32px; }
    .tools-label { font-size: 10px; color: var(--text-4); letter-spacing: 0.1em; text-transform: uppercase; margin-bottom: 12px; }
    .tool { font-size: 12px; color: var(--text-3); padding: 4px 0; }
    .tool-name { color: var(--text-2); }
    .tool-group { margin-bottom: 12px; }
    .group-label { font-size: 10px; color: var(--text-4); margin-bottom: 4px; }
    .nav { margin-top: 32px; display: flex; justify-content: center; gap: 16px; }
    .nav a { font-size: 11px; color: var(--text-3); text-decoration: none; border-bottom: 1px dotted var(--border); }
    .nav a:hover { color: var(--lavender); border-color: var(--lavender); }
    .badge { display: inline-block; padding: 2px 8px; font-size: 9px; font-weight: 700; letter-spacing: 0.06em; text-transform: uppercase; color: var(--sage); background: rgba(142,207,160,0.1); margin-bottom: 24px; }
    .note { font-size: 11px; color: var(--text-4); margin-top: 8px; }
  </style>
</head>
<body>
  <div class="container">
    <svg class="sigil" width="40" height="40" viewBox="0 0 100 100">
      <circle cx="50" cy="50" r="4" opacity="0.8"/>
      <circle cx="50" cy="50" r="12" opacity="0.15"/>
      <circle cx="50" cy="50" r="24" opacity="0.06"/>
    </svg>
    <h1>rosary</h1>
    <div class="subtitle">autonomous work orchestrator</div>
    <span class="badge">mcp server</span>
    <div class="desc">
      dispatches ai agents to work on beads (tracked issues) across repos.
      scans, triages, dispatches, verifies through a 7-tier pipeline, and creates prs.
      27 mcp tools over streamable http.
    </div>
    <div class="connect-box">
      <div class="connect-label">connect</div>
      <div class="connect-cmd">claude mcp add --transport http rosary https://mcp.rosary.bot/mcp</div>
      <div class="note">requires mtls client certificate</div>
    </div>
    <div class="tools">
      <div class="tools-label">tools (27)</div>
      <div class="tool-group">
        <div class="group-label">beads</div>
        <div class="tool"><span class="tool-name">rsry_bead_create</span> &middot; <span class="tool-name">rsry_bead_update</span> &middot; <span class="tool-name">rsry_bead_close</span> &middot; <span class="tool-name">rsry_bead_comment</span> &middot; <span class="tool-name">rsry_bead_search</span> &middot; <span class="tool-name">rsry_bead_link</span> &middot; <span class="tool-name">rsry_bead_import</span></div>
      </div>
      <div class="tool-group">
        <div class="group-label">dispatch</div>
        <div class="tool"><span class="tool-name">rsry_dispatch</span> &middot; <span class="tool-name">rsry_run_once</span> &middot; <span class="tool-name">rsry_active</span> &middot; <span class="tool-name">rsry_scan</span> &middot; <span class="tool-name">rsry_status</span> &middot; <span class="tool-name">rsry_list_beads</span></div>
      </div>
      <div class="tool-group">
        <div class="group-label">pipeline</div>
        <div class="tool"><span class="tool-name">rsry_pipeline_upsert</span> &middot; <span class="tool-name">rsry_pipeline_query</span> &middot; <span class="tool-name">rsry_dispatch_record</span> &middot; <span class="tool-name">rsry_dispatch_history</span></div>
      </div>
      <div class="tool-group">
        <div class="group-label">workspace</div>
        <div class="tool"><span class="tool-name">rsry_workspace_create</span> &middot; <span class="tool-name">rsry_workspace_checkpoint</span> &middot; <span class="tool-name">rsry_workspace_cleanup</span> &middot; <span class="tool-name">rsry_workspace_merge</span></div>
      </div>
      <div class="tool-group">
        <div class="group-label">hierarchy</div>
        <div class="tool"><span class="tool-name">rsry_decade_list</span> &middot; <span class="tool-name">rsry_thread_list</span> &middot; <span class="tool-name">rsry_thread_assign</span> &middot; <span class="tool-name">rsry_decompose</span></div>
      </div>
      <div class="tool-group">
        <div class="group-label">repos</div>
        <div class="tool"><span class="tool-name">rsry_repo_register</span> &middot; <span class="tool-name">rsry_repo_list</span></div>
      </div>
    </div>
    <div class="nav">
      <a href="https://rosary.bot">rosary.bot</a>
      <a href="https://rosary.bot/about">about</a>
      <a href="https://github.com/agentic-research/rosary">source</a>
    </div>
  </div>
</body>
</html>"##;
