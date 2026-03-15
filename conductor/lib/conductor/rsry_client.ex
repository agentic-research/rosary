defmodule Conductor.RsryClient do
  @moduledoc """
  JSON-RPC client to rsry's MCP HTTP endpoint.

  Wraps MCP tools/call into Elixir functions. Maintains a session ID
  for the lifetime of the connection. Reconnects automatically on
  session loss.

  ## Testability

  The client module is configurable via `:rsry_client_mod` in app config.
  In tests, set it to a mock module implementing the same API. In production
  it defaults to this module.
  """
  use GenServer
  require Logger

  defstruct [:url, :session_id, :req, reconnect_attempts: 0]

  @max_reconnect_attempts 5
  @reconnect_backoff_ms 2_000

  # -- Behaviour for test doubles --

  @callback scan() :: {:ok, map()} | {:error, term()}
  @callback status() :: {:ok, map()} | {:error, term()}
  @callback list_beads(String.t() | nil) :: {:ok, map()} | {:error, term()}
  @callback active() :: {:ok, map()} | {:error, term()}
  @callback dispatch(String.t(), String.t(), map()) :: {:ok, map()} | {:error, term()}
  @callback bead_close(String.t(), String.t()) :: {:ok, map()} | {:error, term()}
  @callback bead_comment(String.t(), String.t(), String.t()) :: {:ok, map()} | {:error, term()}
  @callback bead_search(String.t(), String.t()) :: {:ok, map()} | {:error, term()}

  # -- Public API --

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: __MODULE__)
  end

  def scan, do: call_tool("rsry_scan", %{})
  def status, do: call_tool("rsry_status", %{})

  def list_beads(status \\ nil) do
    call_tool("rsry_list_beads", if(status, do: %{status: status}, else: %{}))
  end

  def active, do: call_tool("rsry_active", %{})

  def dispatch(bead_id, repo_path, opts \\ %{}) do
    args = Map.merge(%{bead_id: bead_id, repo_path: repo_path}, opts)
    call_tool("rsry_dispatch", args)
  end

  def bead_close(repo_path, id) do
    call_tool("rsry_bead_close", %{repo_path: repo_path, id: id})
  end

  def bead_comment(repo_path, id, body) do
    call_tool("rsry_bead_comment", %{repo_path: repo_path, id: id, body: body})
  end

  def bead_search(repo_path, query) do
    call_tool("rsry_bead_search", %{repo_path: repo_path, query: query})
  end

  @doc "Check if the client has an active session."
  def connected? do
    GenServer.call(__MODULE__, :connected?)
  catch
    :exit, _ -> false
  end

  defp call_tool(name, args) do
    GenServer.call(__MODULE__, {:tool, name, args}, 30_000)
  catch
    :exit, {:timeout, _} -> {:error, :timeout}
    :exit, reason -> {:error, {:client_down, reason}}
  end

  # -- GenServer callbacks --

  @impl true
  def init(opts) do
    url = opts[:url] || Application.get_env(:conductor, :rsry_url)

    req =
      Req.new(
        base_url: url,
        headers: [
          {"accept", "application/json, text/event-stream"},
          {"content-type", "application/json"}
        ],
        receive_timeout: 30_000
      )

    case initialize(req) do
      {:ok, session_id} ->
        Logger.info("[rsry] connected, session=#{session_id}")
        {:ok, %__MODULE__{url: url, session_id: session_id, req: req}}

      {:error, reason} ->
        Logger.error("[rsry] failed to connect: #{inspect(reason)}")
        # Start anyway — will retry on first call
        {:ok, %__MODULE__{url: url, session_id: nil, req: req}}
    end
  end

  @impl true
  def handle_call(:connected?, _from, state) do
    {:reply, state.session_id != nil, state}
  end

  @impl true
  def handle_call({:tool, name, args}, _from, state) do
    # Try to reconnect if no session
    state = maybe_reconnect(state)

    case do_call_tool(state, name, args) do
      {:ok, result} ->
        {:reply, {:ok, result}, %{state | reconnect_attempts: 0}}

      {:error, {:session_expired, _}} ->
        # Session expired — reconnect and retry once
        Logger.info("[rsry] session expired, reconnecting...")
        state = force_reconnect(state)

        case do_call_tool(state, name, args) do
          {:ok, result} -> {:reply, {:ok, result}, state}
          error -> {:reply, error, state}
        end

      {:error, _} = err ->
        {:reply, err, state}
    end
  end

  # -- MCP protocol --

  defp initialize(req) do
    body = %{
      jsonrpc: "2.0",
      id: 1,
      method: "initialize",
      params: %{
        protocolVersion: "2024-11-05",
        capabilities: %{},
        clientInfo: %{name: "conductor", version: "0.1.0"}
      }
    }

    case Req.post(req, json: body) do
      {:ok, %{status: 200, headers: headers, body: resp}} ->
        session_id =
          headers
          |> Enum.find_value(fn
            {"mcp-session-id", v} -> v
            _ -> nil
          end)

        {:ok, session_id || resp["id"]}

      {:ok, resp} ->
        {:error, {:unexpected_status, resp.status}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp do_call_tool(state, name, args) do
    body = %{
      jsonrpc: "2.0",
      id: System.unique_integer([:positive]),
      method: "tools/call",
      params: %{name: name, arguments: args}
    }

    headers =
      if state.session_id,
        do: [{"mcp-session-id", state.session_id}],
        else: []

    case Req.post(state.req, json: body, headers: headers) do
      {:ok, %{status: 200, body: %{"result" => result}}} ->
        parse_mcp_result(result)

      {:ok, %{status: 200, body: %{"error" => error}}} ->
        {:error, {:mcp_error, error}}

      {:ok, %{status: 404}} ->
        {:error, {:session_expired, "404 — session not found"}}

      {:ok, %{status: status, body: body}} ->
        {:error, {:unexpected_status, status, body}}

      {:error, %Req.TransportError{reason: reason}} ->
        {:error, {:transport, reason}}

      {:error, reason} ->
        {:error, reason}
    end
  end

  defp parse_mcp_result(%{"content" => [%{"text" => text} | _]}) do
    case Jason.decode(text) do
      {:ok, decoded} -> {:ok, decoded}
      {:error, _} -> {:ok, text}
    end
  end

  defp parse_mcp_result(other), do: {:ok, other}

  defp maybe_reconnect(%{session_id: nil} = state), do: force_reconnect(state)
  defp maybe_reconnect(state), do: state

  defp force_reconnect(%{reconnect_attempts: n} = state) when n >= @max_reconnect_attempts do
    Logger.error("[rsry] max reconnect attempts (#{@max_reconnect_attempts}) exhausted")
    state
  end

  defp force_reconnect(state) do
    backoff = @reconnect_backoff_ms * (state.reconnect_attempts + 1)
    Process.sleep(min(backoff, 10_000))

    case initialize(state.req) do
      {:ok, session_id} ->
        Logger.info("[rsry] reconnected, session=#{session_id}")
        %{state | session_id: session_id, reconnect_attempts: 0}

      {:error, reason} ->
        Logger.error("[rsry] reconnect failed: #{inspect(reason)}")
        %{state | reconnect_attempts: state.reconnect_attempts + 1}
    end
  end
end
