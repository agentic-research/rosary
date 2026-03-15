import Config

config :conductor,
  rsry_url: System.get_env("RSRY_URL", "http://127.0.0.1:8383/mcp")
