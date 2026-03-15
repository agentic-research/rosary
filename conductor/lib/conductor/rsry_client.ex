defmodule Conductor.RsryClient do
  @moduledoc """
  JSON-RPC client to rsry's MCP HTTP endpoint.

  Wraps MCP tools/call into Elixir functions. Maintains a session ID
  for the lifetime of the connection.
  """
  use GenServer
  require Logger

  defstruct [:url, :session_id, :req]

  # -- Public API --

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: __MODULE__)
  end

  def scan, do: call_tool("rsry_scan", %{})
  def status, do: call_tool("rsry_status", %{})
  def list_beads(status \\ nil), do: call_tool("rsry_list_beads", if(status, do: %{status: status}, else: %{}))
  def active, do: call_tool("rsry_active", %{})

  def dispatch(bead_id, repo_path, opts \\ %{}) do
    args = Map.merge(%{bead_id: bead_id, repo_path: repo_path}, opts)
    call_tool("rsry_dispatch", args)
  end

  def bead_close(repo_path, id), do: call_tool("rsry_bead_close", %{repo_path: repo_path, id: id})
  def bead_comment(repo_path, id, body), do: call_tool("rsry_bead_comment", %{repo_path: repo_path, id: id, body: body})
  def bead_search(repo_path, query), do: call_tool("rsry_bead_search", %{repo_path: repo_path, query: query})

  defp call_tool(name, args) do
    GenServer.call(__MODULE__, {:tool, name, args}, 30_000)
  end

  # -- GenServer callbacks --

  @impl true
  def init(opts) do
    url = opts[:url] || Application.get_env(:conductor, :rsry_url)
    req = Req.new(
      base_url: url,
      headers: [
        {"accept", "application/json, text/event-stream"},
        {"content-type", "application/json"}
      ]
    )

    case initialize(req) do
      {:ok, session_id} ->
        Logger.info("[rsry] connected, session=#{session_id}")
        {:ok, %__MODULE__{url: url, session_id: session_id, req: req}}

      {:error, reason} ->
        Logger.error("[rsry] failed to connect: #{inspect(reason)}")
        {:ok, %__MODULE__{url: url, session_id: nil, req: req}}
    end
  end

  @impl true
  def handle_call({:tool, name, args}, _from, state) do
    case do_call_tool(state, name, args) do
      {:ok, result} -> {:reply, {:ok, result}, state}
      {:error, _} = err -> {:reply, err, state}
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
        # MCP tools/call returns {content: [{type: "text", text: json_string}]}
        case result do
          %{"content" => [%{"text" => text} | _]} ->
            {:ok, Jason.decode!(text)}

          other ->
            {:ok, other}
        end

      {:ok, %{status: 200, body: %{"error" => error}}} ->
        {:error, error}

      {:ok, resp} ->
        {:error, {:unexpected_status, resp.status, resp.body}}

      {:error, reason} ->
        {:error, reason}
    end
  end
end
