defmodule Conductor.AcpClient do
  @moduledoc """
  ACP (Agent Client Protocol) client over Erlang Port.

  Speaks JSON-RPC over stdio to any ACP-compatible agent binary
  (Claude, Gemini, custom agents). The conductor OWNS the process
  as an Erlang Port — real exit codes, instant completion, clean kill.

  ## Provider-agnostic

  The `binary` field determines which agent runs. ACP is the protocol,
  not tied to any specific LLM:

      AcpClient.start("claude", work_dir, prompt, policy)   # Claude Code
      AcpClient.start("gemini", work_dir, prompt, policy)   # Gemini CLI
      AcpClient.start("my-agent", work_dir, prompt, policy) # Custom

  ## Permission callbacks

  When the agent requests permission to use a tool, the Port sends us
  a `session/request_permission` JSON-RPC message. We evaluate against
  the pipeline's permission policy and respond — no CLI flag strings.

  This is the foundation for sigpol: today `policy_allows?/2` is a
  hardcoded match. Tomorrow it checks a signed policy bundle.
  """
  require Logger

  @type policy :: :read_only | :implement | :plan

  @doc """
  Start an ACP agent process. Returns `{:ok, port, os_pid}`.

  The `binary` is the ACP-compatible executable name (e.g., "claude").
  The `notify_pid` receives `{:acp, event}` messages for lifecycle events.
  """
  def start(binary, work_dir, opts \\ []) do
    executable = System.find_executable(binary) || binary

    args = acp_args(binary)

    port =
      Port.open(
        {:spawn_executable, executable},
        [
          :binary,
          :exit_status,
          {:line, 65_536},
          args: args,
          cd: to_charlist(work_dir)
        ]
      )

    {:os_pid, os_pid} = Port.info(port, :os_pid)
    Logger.info("[acp] started #{binary} (pid=#{os_pid})")

    # Initialize the ACP session
    send_jsonrpc(port, 0, "initialize", %{
      protocolVersion: 1,
      clientCapabilities: %{
        fs: %{readTextFile: true, writeTextFile: true},
        terminal: true
      },
      clientInfo: %{
        name: "conductor",
        title: "Rosary Conductor",
        version: "0.1.0"
      }
    })

    notify_pid = opts[:notify_pid]
    if notify_pid, do: send(notify_pid, {:acp, :initialized, os_pid})

    {:ok, port, os_pid}
  end

  @doc "Create a new session and send a prompt."
  def prompt(port, session_id \\ nil, work_dir, prompt_text) do
    # Create session if needed
    sid =
      if session_id do
        session_id
      else
        send_jsonrpc(port, 1, "session/new", %{workingDirectory: work_dir})
        # Session ID comes back in the response — for now use a placeholder
        "session-#{System.unique_integer([:positive])}"
      end

    send_jsonrpc(port, 2, "session/prompt", %{
      sessionId: sid,
      prompt: [%{type: "text", text: prompt_text}]
    })

    sid
  end

  @doc """
  Handle an ACP message from the Port. Call this from your GenServer's handle_info.

  Returns `{:ok, event}` where event is one of:
  - `{:permission_request, msg_id, tool_call, options}` — needs response
  - `{:tool_call, tool_call_id, title, kind}` — agent is calling a tool
  - `{:tool_update, tool_call_id, status}` — tool call progress
  - `{:prompt_complete, stop_reason, result}` — agent finished
  - `{:update, data}` — streaming update
  - `{:unknown, data}` — unrecognized message
  """
  def handle_message(line) when is_binary(line) do
    case Jason.decode(line) do
      {:ok, msg} -> parse_acp_message(msg)
      {:error, _} -> {:ok, {:unknown, line}}
    end
  end

  @doc "Respond to a permission request."
  def approve(port, msg_id, option_id \\ "allow-once") do
    send_jsonrpc_response(port, msg_id, %{
      outcome: %{outcome: "selected", optionId: option_id}
    })
  end

  @doc "Reject a permission request."
  def reject(port, msg_id, option_id \\ "reject-once") do
    send_jsonrpc_response(port, msg_id, %{
      outcome: %{outcome: "selected", optionId: option_id}
    })
  end

  @doc """
  Evaluate whether a tool should be allowed based on the permission policy.

  This is the local policy check — same pattern as sigpol's PolicyChecker.
  Today it's a hardcoded match. Tomorrow it checks a signed bundle.
  """
  @spec policy_allows?(String.t(), policy()) :: boolean()
  def policy_allows?(tool_name, policy) do
    is_mcp =
      String.starts_with?(tool_name, "mcp__mache__") or
        String.starts_with?(tool_name, "mcp__rsry__")

    is_read = tool_name in ["Read", "Glob", "Grep"]

    case policy do
      :read_only ->
        is_read or is_mcp

      :implement ->
        is_read or is_mcp or tool_name in ["Edit", "Write"] or
          String.starts_with?(tool_name, "Bash")

      :plan ->
        is_read or is_mcp
    end
  end

  @doc """
  Map issue_type to permission policy. Can be overridden by step mode.

  Step modes take precedence:
  - `:plan_first` → `:read_only` during planning, `:implement` after approval
  - `:read_only` → `:read_only` always
  - `:implement` → determined by issue_type
  """
  @spec policy_for(String.t(), atom()) :: policy()
  def policy_for(issue_type, step_mode \\ :implement) do
    case step_mode do
      :read_only -> :read_only
      :plan_first -> :read_only
      :implement ->
        case issue_type do
          t when t in ["review", "survey", "audit"] -> :read_only
          t when t in ["epic", "plan", "triage", "design", "research"] -> :plan
          _ -> :implement
        end
    end
  end

  # -- Private --

  defp acp_args(binary) do
    case binary do
      "claude" -> ["--acp"]
      "gemini" -> ["--acp"]
      _ -> []
    end
  end

  defp parse_acp_message(%{"method" => "session/request_permission", "id" => id, "params" => params}) do
    tool_call = params["toolCall"] || %{}
    options = params["options"] || []
    {:ok, {:permission_request, id, tool_call, options}}
  end

  defp parse_acp_message(%{"method" => "session/update", "params" => %{"update" => update}}) do
    case update do
      %{"sessionUpdate" => "tool_call", "toolCallId" => id, "title" => title} ->
        {:ok, {:tool_call, id, title, update["kind"]}}

      %{"sessionUpdate" => "tool_call_update", "toolCallId" => id} ->
        {:ok, {:tool_update, id, update["status"]}}

      _ ->
        {:ok, {:update, update}}
    end
  end

  defp parse_acp_message(%{"id" => _id, "result" => %{"stopReason" => reason} = result}) do
    {:ok, {:prompt_complete, reason, result}}
  end

  defp parse_acp_message(%{"id" => _id, "result" => result}) do
    {:ok, {:response, result}}
  end

  defp parse_acp_message(other) do
    {:ok, {:unknown, other}}
  end

  defp send_jsonrpc(port, id, method, params) do
    msg = Jason.encode!(%{jsonrpc: "2.0", id: id, method: method, params: params})
    Port.command(port, msg <> "\n")
  end

  defp send_jsonrpc_response(port, id, result) do
    msg = Jason.encode!(%{jsonrpc: "2.0", id: id, result: result})
    Port.command(port, msg <> "\n")
  end
end
