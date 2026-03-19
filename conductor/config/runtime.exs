import Config

config :conductor,
  rsry_url: System.get_env("RSRY_URL", "http://127.0.0.1:8383/mcp"),
  compute_backend: (System.get_env("CONDUCTOR_COMPUTE") || "local") |> String.to_atom(),
  sprites_token: System.get_env("SPRITES_TOKEN"),
  sprites_base_url: System.get_env("SPRITES_BASE_URL", "https://api.sprites.dev/v1"),
  anthropic_api_key: System.get_env("ANTHROPIC_API_KEY"),
  github_token: System.get_env("GITHUB_TOKEN")
