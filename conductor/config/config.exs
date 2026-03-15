import Config

config :conductor,
  rsry_url: "http://127.0.0.1:8383/mcp",
  scan_interval_ms: 30_000,
  agent_timeout_ms: 10 * 60_000,
  max_concurrent: 3
