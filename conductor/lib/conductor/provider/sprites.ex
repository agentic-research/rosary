defmodule Conductor.Provider.Sprites do
  @moduledoc """
  Sprites VM compute provider. Runs agent processes on remote Sprites VMs
  via REST API (provisioning) and WebSocket (streaming exec).

  Supports both ACP and CLI protocols — the protocol is orthogonal to
  where the process runs. ACP JSON-RPC flows through `send_input/2` which
  writes to the WebSocket stdin channel.

  ## Lifecycle

  1. `provision/3` — creates sprite, sets network policy, clones repo
  2. `spawn_process/6` — starts SpritesExec WebSocket for streaming
  3. Agent runs inside the sprite, producing Port-compatible messages
  4. `deprovision/1` — destroys the sprite

  Sprites persist across retries (preserving agent work). Only `deprovision/1`
  destroys them — called on pipeline completion or deadletter.
  """
  @behaviour Conductor.Provider

  require Logger

  @impl true
  def provision(name, repo, opts) do
    client = sprites_client()
    github_token = Application.get_env(:conductor, :github_token)

    with {:ok, _} <- client.create_sprite(name, opts),
         :ok <- client.set_network_policy(name),
         clone_cmd =
           "git clone --depth=1 https://#{github_token}@github.com/#{repo_to_github_path(repo)} /workspace",
         {:ok, _} <- client.exec_sync(name, clone_cmd, %{}) do
      Logger.info("[provider:sprites] provisioned #{name} with repo #{repo}")
      :ok
    end
  end

  @impl true
  def spawn_process(name, binary, args, work_dir, env, worker_pid) do
    # Build the full command: cd to work_dir then exec binary with args
    command =
      "cd #{shell_escape(work_dir)} && #{shell_escape(binary)} " <>
        Enum.map_join(args, " ", &shell_escape/1)

    # Merge in the ANTHROPIC_API_KEY for API-key-based protocols (ACP)
    anthropic_key = Application.get_env(:conductor, :anthropic_api_key)

    full_env =
      env
      |> Map.put("ANTHROPIC_API_KEY", anthropic_key || "")

    case Conductor.SpritesExec.start_link(
           sprite_name: name,
           worker_pid: worker_pid,
           command: command,
           env: full_env
         ) do
      {:ok, pid} ->
        Logger.info("[provider:sprites] spawned process on #{name} (exec=#{inspect(pid)})")
        {:ok, pid, name}

      {:error, reason} ->
        {:error, reason}
    end
  end

  @impl true
  def send_input(pid, data) when is_pid(pid) do
    Conductor.SpritesExec.send_input(pid, data)
  end

  @impl true
  def stop_process(pid) when is_pid(pid) do
    if Process.alive?(pid) do
      Process.exit(pid, :shutdown)
    end

    :ok
  end

  @impl true
  def alive?(pid) when is_pid(pid), do: Process.alive?(pid)
  def alive?(_), do: false

  @impl true
  def exec_sync(name, command, _work_dir) do
    # Validation runs inside the sprite via REST sync exec
    client = sprites_client()

    case client.exec_sync(name, command, %{}) do
      {:ok, %{"exit_code" => code, "stdout" => stdout}} ->
        {:ok, {stdout, code}}

      {:ok, %{"exit_code" => code}} ->
        {:ok, {"", code}}

      {:ok, result} ->
        # Flexible: handle varying response shapes
        code = Map.get(result, "exit_code", Map.get(result, "code", 1))
        output = Map.get(result, "stdout", Map.get(result, "output", ""))
        {:ok, {output, code}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  @impl true
  def deprovision(name) do
    client = sprites_client()
    Logger.info("[provider:sprites] deprovisioning #{name}")
    client.destroy_sprite(name)
    :ok
  end

  # -- Helpers --

  defp sprites_client do
    Application.get_env(:conductor, :sprites_client_mod, Conductor.SpritesClient)
  end

  defp repo_to_github_path(repo) do
    repo
    |> String.trim_trailing("/")
    |> String.split("/")
    |> Enum.take(-2)
    |> Enum.join("/")
  end

  defp shell_escape(arg) do
    if String.contains?(arg, [" ", "'", "\"", "\n", "\t", ";", "&", "|", "(", ")"]) do
      "'" <> String.replace(arg, "'", "'\\''") <> "'"
    else
      arg
    end
  end
end
