defmodule Conductor.SpritesExec do
  @moduledoc """
  WebSocket GenServer that manages a streaming exec session on a Sprites VM.

  Connects to `WSS /sprites/{name}/exec`, sends the command+env as the first
  message, then translates sprite stdout/exit into Port-compatible messages
  that AgentWorker can consume without modification:

      Sprite WSS binary (0x01 prefix) → {self(), {:data, {:eol, line}}}
      Sprite WSS exit JSON             → {self(), {:exit_status, code}}

  This allows AgentWorker to store the SpritesExec pid in `state.port` and
  have all existing `handle_info` pattern matches work unchanged — Erlang
  treats pids and port refs as opaque terms in match heads.

  ## Options

  - `:sprite_name` — Name of the sprite (required)
  - `:worker_pid` — PID of the AgentWorker to receive messages (required)
  - `:command` — Shell command to execute (required)
  - `:env` — Environment variables map (default: %{})
  - `:token` — Sprites API token (default: from app config)
  """
  use WebSockex
  require Logger

  defstruct [:worker_pid, :sprite_name, :command, :env, buffer: "", sent_command: false]

  @doc """
  Start a WebSocket connection to a sprite's exec endpoint.

  Returns `{:ok, pid}` where `pid` can be stored as `state.port` in AgentWorker.
  """
  def start_link(opts) do
    sprite_name = Keyword.fetch!(opts, :sprite_name)
    worker_pid = Keyword.fetch!(opts, :worker_pid)
    command = Keyword.fetch!(opts, :command)
    env = Keyword.get(opts, :env, %{})
    token = Keyword.get(opts, :token) || Application.get_env(:conductor, :sprites_token, "")

    url = sprites_client().exec_ws_url(sprite_name)

    state = %__MODULE__{
      worker_pid: worker_pid,
      sprite_name: sprite_name,
      command: command,
      env: env
    }

    WebSockex.start_link(url, __MODULE__, state,
      extra_headers: [{"authorization", "Bearer #{token}"}]
    )
  end

  @doc """
  Send data to the remote process stdin via WebSocket.

  Uses 0x00 prefix to signal stdin data to the Sprites exec endpoint,
  mirroring the 0x01 (stdout) / 0x02 (stderr) output prefixes.
  """
  def send_input(pid, data) do
    binary_data = IO.iodata_to_binary(data)
    WebSockex.send_frame(pid, {:binary, <<0x00, binary_data::binary>>})
  end

  defp sprites_client do
    Application.get_env(:conductor, :sprites_client_mod, Conductor.SpritesClient)
  end

  # -- WebSockex Callbacks --

  @impl true
  def handle_connect(_conn, state) do
    Logger.info("[sprites_exec] #{state.sprite_name}: connected, sending command")

    frame =
      Jason.encode!(%{
        "command" => state.command,
        "env" => state.env
      })

    {:reply, {:text, frame}, %{state | sent_command: true}}
  end

  @impl true
  def handle_frame({:binary, <<0x01, data::binary>>}, state) do
    # Stdout data — buffer and emit complete lines
    {lines, remaining} = split_lines(state.buffer <> data)

    for line <- lines do
      send(state.worker_pid, {self(), {:data, {:eol, line}}})
    end

    {:ok, %{state | buffer: remaining}}
  end

  def handle_frame({:binary, <<0x02, data::binary>>}, state) do
    # Stderr data — also forward as stdout lines to worker
    {lines, remaining} = split_lines(state.buffer <> data)

    for line <- lines do
      send(state.worker_pid, {self(), {:data, {:eol, line}}})
    end

    {:ok, %{state | buffer: remaining}}
  end

  def handle_frame({:binary, _data}, state) do
    # Unknown binary prefix — ignore
    {:ok, state}
  end

  def handle_frame({:text, text}, state) do
    case Jason.decode(text) do
      {:ok, %{"type" => "exit", "exit_code" => code}} ->
        handle_exit(code, state)

      {:ok, %{"type" => "exit", "code" => code}} ->
        handle_exit(code, state)

      {:ok, _other} ->
        {:ok, state}

      {:error, _} ->
        {:ok, state}
    end
  end

  def handle_frame(_frame, state), do: {:ok, state}

  @impl true
  def handle_disconnect(%{reason: {:remote, code, _}}, state) do
    Logger.warning("[sprites_exec] #{state.sprite_name}: disconnected (code=#{code})")

    # If we never got an exit frame, treat disconnect as crash
    send(state.worker_pid, {self(), {:exit_status, 1}})
    {:ok, state}
  end

  def handle_disconnect(%{reason: reason}, state) do
    Logger.warning("[sprites_exec] #{state.sprite_name}: disconnected (#{inspect(reason)})")

    send(state.worker_pid, {self(), {:exit_status, 1}})
    {:ok, state}
  end

  @impl true
  def terminate(reason, state) do
    Logger.info("[sprites_exec] #{state.sprite_name}: terminated (#{inspect(reason)})")
    :ok
  end

  # -- Internal --

  defp handle_exit(code, state) do
    # Flush remaining buffer as final line
    if state.buffer != "" do
      send(state.worker_pid, {self(), {:data, {:eol, state.buffer}}})
    end

    send(state.worker_pid, {self(), {:exit_status, code}})

    {:close, %{state | buffer: ""}}
  end

  @doc false
  def split_lines(data) do
    parts = String.split(data, "\n", parts: :infinity)

    case parts do
      [single] ->
        # No newline found — everything is remainder
        {[], single}

      _ ->
        # Last element is the remainder (possibly empty string after trailing \n)
        {complete, [remainder]} = Enum.split(parts, -1)
        {complete, remainder}
    end
  end
end
