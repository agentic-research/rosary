defmodule Conductor.AgentWorker do
  @moduledoc """
  GenServer managing a bead's full pipeline execution.

  The conductor OWNS the agent process as an Erlang Port — not monitoring
  a PID spawned by rsry. This gives us:
  - Real exit codes (not polling approximations)
  - Instant `:exit_status` messages (no polling)
  - Clean cleanup on terminate (Port.close sends SIGTERM)

  The pipeline (closure) is the state. Phase advancement is synchronous
  within message handlers — no window for lost continuations.
  """
  use GenServer, restart: :temporary
  require Logger

  alias Conductor.Pipeline

  # Configurable for testing
  defp client, do: Application.get_env(:conductor, :rsry_client_mod, Conductor.RsryClient)
  defp spawn_fn, do: Application.get_env(:conductor, :agent_spawn_fn, nil)

  defstruct [
    :pipeline,
    :port,
    :os_pid,
    :timeout_ref,
    :started_at,
    :work_dir,
    pending_tools: %{}
  ]

  # -- Public API --

  def start_link(bead) do
    GenServer.start_link(__MODULE__, bead)
  end

  def get_state(pid), do: GenServer.call(pid, :get_state)

  # -- GenServer callbacks --

  @impl true
  def init(bead) do
    bead_id = bead["id"] || bead[:id]
    repo = bead["repo"] || bead[:repo]
    issue_type = bead["issue_type"] || bead[:issue_type] || "task"
    owner = bead["owner"] || bead[:owner]

    pipeline =
      if owner do
        Pipeline.for_bead(bead_id, repo, issue_type, owner)
      else
        Pipeline.for_bead(bead_id, repo, issue_type)
      end

    Logger.info(
      "[worker] #{bead_id}: pipeline #{inspect(Pipeline.agents(pipeline))} " <>
        "(starting at #{Pipeline.current_agent(pipeline)})"
    )

    case start_agent_process(pipeline) do
      {:ok, port, os_pid} ->
        step = Pipeline.current_step(pipeline)
        timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)

        {:ok,
         %__MODULE__{
           pipeline: pipeline,
           port: port,
           os_pid: os_pid,
           timeout_ref: timeout_ref,
           started_at: DateTime.utc_now(),
           work_dir: repo
         }}

      {:error, reason} ->
        Logger.error("[worker] #{bead_id}: failed to start agent: #{inspect(reason)}")
        {:stop, {:start_failed, reason}}
    end
  end

  # -- Agent process exited (real exit code from Port) --

  @impl true
  def handle_info({port, {:exit_status, code}}, %{port: port} = state) do
    if state.timeout_ref, do: Process.cancel_timer(state.timeout_ref)

    elapsed = DateTime.diff(DateTime.utc_now(), state.started_at, :second)
    agent = Pipeline.current_agent(state.pipeline)
    bead_id = state.pipeline.bead_id

    Logger.info("[worker] #{bead_id}: #{agent} exited (code=#{code}, #{elapsed}s)")

    if code == 0 do
      on_success(state)
    else
      on_failure(state, code)
    end
  end

  # -- Timeout --

  @impl true
  def handle_info(:timeout, state) do
    bead_id = state.pipeline.bead_id
    agent = Pipeline.current_agent(state.pipeline)
    step = Pipeline.current_step(state.pipeline)
    Logger.warning("[timeout] #{bead_id}: killing #{agent} (pid=#{state.os_pid})")

    if state.port, do: Port.close(state.port)

    pipeline = Pipeline.record(state.pipeline, :timeout, "exceeded #{step.timeout_ms}ms")
    # Port close will trigger exit_status message
    {:noreply, %{state | pipeline: pipeline}}
  end

  # -- Retry (after backoff) --

  @impl true
  def handle_info(:retry, state) do
    case start_agent_process(state.pipeline) do
      {:ok, port, os_pid} ->
        step = Pipeline.current_step(state.pipeline)
        timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)

        {:noreply,
         %{state |
           port: port,
           os_pid: os_pid,
           timeout_ref: timeout_ref,
           started_at: DateTime.utc_now()
         }}

      {:error, reason} ->
        Logger.error("[retry] #{state.pipeline.bead_id}: start failed: #{inspect(reason)}")
        {:stop, {:retry_start_failed, reason}, state}
    end
  end

  @impl true
  def handle_info({port, {:data, {:eol, line}}}, %{port: port} = state) do
    # ACP JSON-RPC message from the agent
    handle_acp_message(line, state)
  end

  @impl true
  def handle_info({port, {:data, {:noeol, _chunk}}}, %{port: port} = state) do
    # Partial line — ignore (will get full line on next :eol)
    {:noreply, state}
  end

  @impl true
  def handle_info({_port, {:data, _data}}, state) do
    # Non-line data from the Port
    {:noreply, state}
  end

  @impl true
  def handle_info(_msg, state), do: {:noreply, state}

  @impl true
  def handle_call(:get_state, _from, state) do
    {:reply,
     %{
       bead_id: state.pipeline.bead_id,
       pipeline: Pipeline.to_map(state.pipeline),
       progress: Pipeline.progress(state.pipeline),
       os_pid: state.os_pid,
       started_at: state.started_at
     }, state}
  end

  @impl true
  def terminate(reason, state) do
    bead_id = state.pipeline.bead_id
    {done, total} = Pipeline.progress(state.pipeline)
    Logger.info("[worker] #{bead_id}: terminated (#{done}/#{total} phases, reason=#{inspect(reason)})")

    # Clean up the Port if still open
    if state.port && Port.info(state.port) != nil do
      Port.close(state.port)
    end

    :ok
  end

  # -- Phase progression --

  defp on_success(state) do
    pipeline = Pipeline.record(state.pipeline, :pass)
    bead_id = pipeline.bead_id
    agent = Pipeline.current_agent(pipeline)

    case Pipeline.advance(pipeline) do
      :done ->
        Logger.info("[pipeline] #{bead_id}: complete (#{Enum.join(Pipeline.agents(pipeline), " → ")})")

        client().bead_comment(
          pipeline.repo,
          bead_id,
          "Pipeline complete: #{Enum.join(Pipeline.agents(pipeline), " → ")}"
        )

        {:stop, :normal, %{state | pipeline: pipeline}}

      {:next, next_pipeline} ->
        next_agent = Pipeline.current_agent(next_pipeline)
        Logger.info("[pipeline] #{bead_id}: #{agent} passed → #{next_agent}")

        client().bead_comment(
          pipeline.repo,
          bead_id,
          "Phase passed: #{agent} → advancing to #{next_agent}"
        )

        case start_agent_process(next_pipeline) do
          {:ok, port, os_pid} ->
            step = Pipeline.current_step(next_pipeline)
            timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)

            {:noreply,
             %{state |
               pipeline: next_pipeline,
               port: port,
               os_pid: os_pid,
               timeout_ref: timeout_ref,
               started_at: DateTime.utc_now()
             }}

          {:error, reason} ->
            Logger.error("[pipeline] #{bead_id}: #{next_agent} start failed: #{inspect(reason)}")
            {:stop, {:phase_start_failed, reason}, %{state | pipeline: next_pipeline}}
        end
    end
  end

  defp on_failure(state, exit_code) do
    pipeline = Pipeline.record(state.pipeline, :fail, "exit code #{exit_code}")
    bead_id = pipeline.bead_id
    agent = Pipeline.current_agent(pipeline)

    if Pipeline.can_retry?(pipeline) do
      retries = Pipeline.retries_used(pipeline)
      step = Pipeline.current_step(pipeline)
      Logger.info("[retry] #{bead_id}: #{agent} retry #{retries}/#{step.max_retries}")

      client().bead_comment(
        pipeline.repo,
        bead_id,
        "#{agent} failed (exit #{exit_code}), retrying (#{retries}/#{step.max_retries})"
      )

      backoff = min(30_000 * :math.pow(2, retries) |> trunc(), 300_000)
      Process.send_after(self(), :retry, backoff)
      {:noreply, %{state | pipeline: pipeline, port: nil, os_pid: nil, timeout_ref: nil}}
    else
      Logger.warning("[deadletter] #{bead_id}: #{agent} exhausted retries")

      client().bead_comment(
        pipeline.repo,
        bead_id,
        "#{agent} exhausted retries — deadlettered"
      )

      {:stop, :normal, %{state | pipeline: pipeline}}
    end
  end

  # -- Agent process spawning --

  defp start_agent_process(pipeline) do
    # Allow test injection of a custom spawn function
    case spawn_fn() do
      nil -> start_acp_process(pipeline)
      fun when is_function(fun) -> fun.(pipeline)
    end
  end

  defp start_acp_process(pipeline) do
    alias Conductor.AcpClient

    agent = Pipeline.current_agent(pipeline)
    bead_id = pipeline.bead_id
    repo = pipeline.repo

    # Resolve binary from agent name (default: claude)
    binary = agent_binary(agent)

    prompt = build_prompt(pipeline)

    try do
      {:ok, port, os_pid} = AcpClient.start(binary, repo)
      _session_id = AcpClient.prompt(port, nil, repo, prompt)
      Logger.info("[worker] #{bead_id}: spawned #{agent} via ACP (#{binary}, pid=#{os_pid})")
      {:ok, port, os_pid}
    rescue
      e -> {:error, Exception.message(e)}
    end
  end

  defp build_prompt(pipeline) do
    bead_id = pipeline.bead_id
    repo = pipeline.repo
    agent = Pipeline.current_agent(pipeline)

    "Fix this issue. Make the minimal change needed.\n\n" <>
      "Bead ID: #{bead_id}\n" <>
      "Repo: #{repo}\n" <>
      "Agent: #{agent}\n" <>
      "Title: #{pipeline.issue_type} work\n\n" <>
      "After fixing:\n" <>
      "1. Run tests via `task test`\n" <>
      "2. Create a commit with a descriptive message\n" <>
      "3. Close this bead: call mcp__rsry__rsry_bead_close with repo_path=\"#{repo}\" and id=\"#{bead_id}\"\n" <>
      "4. Report what you changed"
  end

  # -- ACP message handling --

  defp handle_acp_message(line, state) do
    alias Conductor.AcpClient

    case AcpClient.handle_message(line) do
      {:ok, {:permission_request, msg_id, tool_call, options}} ->
        # Local policy check — the sigpol pattern
        tool_name = tool_call["fields"]["title"] || tool_call["title"] || "unknown"
        tool_id = tool_call["toolCallId"]
        tool_info = Map.get(state.pending_tools, tool_id, %{})
        resolved_name = tool_info[:title] || tool_name

        policy = AcpClient.policy_for(state.pipeline.issue_type)
        approved = AcpClient.policy_allows?(resolved_name, policy)

        option_id =
          if approved do
            find_option(options, "allow_once") || find_option(options, "allow_always")
          else
            find_option(options, "reject_once")
          end

        if approved do
          AcpClient.approve(state.port, msg_id, option_id)
        else
          AcpClient.reject(state.port, msg_id, option_id)
          Logger.info("[policy] #{state.pipeline.bead_id}: rejected #{resolved_name}")
        end

        {:noreply, state}

      {:ok, {:tool_call, tool_id, title, kind}} ->
        # Stash tool details for permission lookup
        tools = Map.put(state.pending_tools, tool_id, %{title: title, kind: kind})
        {:noreply, %{state | pending_tools: tools}}

      {:ok, {:tool_update, _tool_id, _status}} ->
        {:noreply, state}

      {:ok, {:prompt_complete, _reason, _result}} ->
        # Agent finished via ACP — the exit_status handler will also fire
        {:noreply, state}

      {:ok, _other} ->
        {:noreply, state}
    end
  end

  defp find_option(options, kind) do
    case Enum.find(options, fn o -> o["kind"] == kind end) do
      %{"optionId" => id} -> id
      _ -> kind
    end
  end

  # Map to ACP binary. All providers speak the same protocol via --acp.
  # Provider selection: config > per-step override > default (claude)
  @supported_providers ~w(claude gemini codex copilot qwen-code)

  defp agent_binary(_agent_name) do
    provider = Application.get_env(:conductor, :agent_provider, "claude")
    if provider not in @supported_providers do
      Logger.warning("[worker] unknown provider #{provider}, using as-is (custom ACP binary)")
    end
    provider
  end
end
