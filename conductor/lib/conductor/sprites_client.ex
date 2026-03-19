defmodule Conductor.SpritesClient do
  @moduledoc """
  REST client for the Sprites VM API.

  Handles sprite CRUD, synchronous exec (for setup commands like git clone),
  and network policy configuration. Stateless — each call builds a fresh Req request.

  Configure via application env:
  - `:sprites_token` — Bearer token for API auth
  - `:sprites_base_url` — API base URL (default: https://api.sprites.dev/v1)

  For testing, swap the module via `:sprites_client_mod` app env.
  """

  @callback create_sprite(name :: String.t(), opts :: map()) :: {:ok, map()} | {:error, term()}
  @callback destroy_sprite(name :: String.t()) :: :ok | {:error, term()}
  @callback exec_sync(name :: String.t(), command :: String.t(), env :: map()) ::
              {:ok, map()} | {:error, term()}
  @callback set_network_policy(name :: String.t(), policy :: map()) :: :ok | {:error, term()}
  @callback exec_ws_url(name :: String.t()) :: String.t()

  @behaviour __MODULE__

  @default_network_policy %{
    "default" => "deny",
    "allowed_domains" => [
      "api.anthropic.com",
      "github.com",
      "*.githubusercontent.com",
      "registry.npmjs.org",
      "crates.io",
      "static.crates.io"
    ]
  }

  def default_network_policy, do: @default_network_policy

  @impl true
  def create_sprite(name, opts \\ %{}) do
    body = Map.merge(%{"name" => name}, opts)

    case req_put("/sprites", body) do
      {:ok, %{status: status, body: body}} when status in [200, 201] ->
        {:ok, body}

      {:ok, %{status: 409, body: body}} ->
        # Already exists — idempotent
        {:ok, body}

      {:ok, %{status: status, body: body}} ->
        {:error, {:http, status, body}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  @impl true
  def destroy_sprite(name) do
    case req_delete("/sprites/#{name}") do
      {:ok, %{status: status}} when status in [200, 204, 404] ->
        :ok

      {:ok, %{status: status, body: body}} ->
        {:error, {:http, status, body}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  @impl true
  def exec_sync(name, command, env \\ %{}) do
    body = %{"command" => command, "env" => env}

    case req_post("/sprites/#{name}/exec", body) do
      {:ok, %{status: 200, body: body}} ->
        {:ok, body}

      {:ok, %{status: status, body: body}} ->
        {:error, {:http, status, body}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  @impl true
  def set_network_policy(name, policy \\ @default_network_policy) do
    case req_put("/sprites/#{name}/policies/network", policy) do
      {:ok, %{status: status}} when status in [200, 204] ->
        :ok

      {:ok, %{status: status, body: body}} ->
        {:error, {:http, status, body}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  @impl true
  def exec_ws_url(name) do
    base = base_url()

    ws_base =
      base
      |> String.replace_leading("https://", "wss://")
      |> String.replace_leading("http://", "ws://")

    "#{ws_base}/sprites/#{name}/exec"
  end

  @doc "Derive a deterministic sprite name from a bead ID."
  def sprite_name(bead_id), do: "rsry-#{bead_id}"

  # -- HTTP helpers --

  defp req_put(path, body) do
    Req.put(base_url() <> path,
      json: body,
      headers: auth_headers()
    )
  end

  defp req_post(path, body) do
    Req.post(base_url() <> path,
      json: body,
      headers: auth_headers()
    )
  end

  defp req_delete(path) do
    Req.delete(base_url() <> path,
      headers: auth_headers()
    )
  end

  defp base_url do
    Application.get_env(:conductor, :sprites_base_url, "https://api.sprites.dev/v1")
  end

  defp auth_headers do
    token = Application.get_env(:conductor, :sprites_token, "")
    [{"authorization", "Bearer #{token}"}]
  end
end
