import Config

# Disable orchestrator auto-tick in tests
config :conductor,
  scan_interval_ms: :infinity,
  max_concurrent: 0
