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
    :session_id,
    :bead_title,
    :bead_description,
    pending_tools: %{},
    acp_stop_reason: nil,
    validate_ref: nil
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
    repo_raw = bead["repo"] || bead[:repo]
    repo = Conductor.RepoResolver.resolve(repo_raw)
    issue_type = bead["issue_type"] || bead[:issue_type] || "task"
    owner = bead["owner"] || bead[:owner]
    title = bead["title"] || bead[:title] || "#{issue_type} work"
    description = bead["description"] || bead[:description] || ""

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

    # Create isolated workspace via rsry (jj preferred, git fallback)
    work_dir =
      case client().workspace_create(bead_id, repo) do
        {:ok, %{"work_dir" => wd}} ->
          Logger.info("[worker] #{bead_id}: workspace created at #{wd}")
          wd

        {:error, reason} ->
          Logger.warning(
            "[worker] #{bead_id}: workspace create failed (#{inspect(reason)}), running in-place"
          )

          repo
      end

    case start_agent_process(pipeline, nil) do
      {:ok, port, os_pid} ->
        step = Pipeline.current_step(pipeline)
        timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)
        validate_ref = schedule_validation(step)

        {:ok,
         %__MODULE__{
           pipeline: pipeline,
           port: port,
           os_pid: os_pid,
           timeout_ref: timeout_ref,
           validate_ref: validate_ref,
           started_at: DateTime.utc_now(),
           work_dir: work_dir,
           bead_title: title,
           bead_description: description
         }}

      {:error, reason} ->
        Logger.error("[worker] #{bead_id}: failed to start agent: #{inspect(reason)}")
        # Clean up workspace on start failure
        client().workspace_cleanup(bead_id, repo)
        {:stop, {:start_failed, reason}}
    end
  end

  # -- Agent process exited (real exit code from Port) --

  @impl true
  def handle_info({port, {:exit_status, code}}, %{port: port} = state) do
    if state.timeout_ref, do: Process.cancel_timer(state.timeout_ref)
    if state.validate_ref, do: Process.cancel_timer(state.validate_ref)

    elapsed = DateTime.diff(DateTime.utc_now(), state.started_at, :second)
    agent = Pipeline.current_agent(state.pipeline)
    bead_id = state.pipeline.bead_id

    # ACP stop_reason overrides exit code: "refusal" or "max_tokens" = fail even with exit 0
    effective_success =
      case state.acp_stop_reason do
        "end_turn" -> code == 0
        "refusal" -> false
        "max_tokens" -> false
        "cancelled" -> false
        nil -> code == 0
        _ -> code == 0
      end

    Logger.info(
      "[worker] #{bead_id}: #{agent} exited (code=#{code}, acp=#{state.acp_stop_reason || "n/a"}, #{elapsed}s)"
    )

    if effective_success do
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

    if state.validate_ref, do: Process.cancel_timer(state.validate_ref)
    if state.port, do: Port.close(state.port)

    pipeline = Pipeline.record(state.pipeline, :timeout, "exceeded #{step.timeout_ms}ms")
    # Port close will trigger exit_status message
    {:noreply, %{state | pipeline: pipeline, validate_ref: nil}}
  end

  # -- Retry (after backoff) --

  @impl true
  def handle_info(:retry, state) do
    case start_agent_process(state.pipeline, state) do
      {:ok, port, os_pid} ->
        step = Pipeline.current_step(state.pipeline)
        timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)
        validate_ref = schedule_validation(step)

        {:noreply,
         %{
           state
           | port: port,
             os_pid: os_pid,
             timeout_ref: timeout_ref,
             validate_ref: validate_ref,
             started_at: DateTime.utc_now(),
             acp_stop_reason: nil
         }}

      {:error, reason} ->
        Logger.error("[retry] #{state.pipeline.bead_id}: start failed: #{inspect(reason)}")
        {:stop, {:retry_start_failed, reason}, state}
    end
  end

  # -- Validation loop (the built-in /loop) --

  @impl true
  def handle_info(:validate, state) do
    step = Pipeline.current_step(state.pipeline)
    bead_id = state.pipeline.bead_id

    case step && step.validation do
      nil ->
        {:noreply, state}

      %{command: command, on_fail: on_fail} = validation ->
        case run_validation(command, state.work_dir) do
          :pass ->
            ref = Process.send_after(self(), :validate, validation.interval_ms)
            {:noreply, %{state | validate_ref: ref}}

          {:fail, output} ->
            agent = Pipeline.current_agent(state.pipeline)
            Logger.warning("[validate] #{bead_id}: #{command} failed during #{agent}")

            case on_fail do
              :notify_agent ->
                # Inject failure into the active ACP session
                if state.port && state.session_id do
                  alias Conductor.AcpClient

                  AcpClient.resume(
                    state.port,
                    state.session_id,
                    "Tests are failing. Fix before continuing:\n\n```\n#{truncate(output, 2000)}\n```"
                  )
                end

                ref = Process.send_after(self(), :validate, validation.interval_ms)
                {:noreply, %{state | validate_ref: ref}}

              :kill ->
                client().bead_comment(
                  state.pipeline.repo,
                  bead_id,
                  "Validation failed during #{agent}, killing: #{truncate(output, 500)}"
                )

                if state.port, do: Port.close(state.port)
                pipeline = Pipeline.record(state.pipeline, :fail, "validation: #{command}")
                {:noreply, %{state | pipeline: pipeline, validate_ref: nil}}

              :log_only ->
                ref = Process.send_after(self(), :validate, validation.interval_ms)
                {:noreply, %{state | validate_ref: ref}}
            end
        end
    end
  end

  @impl true
  def handle_info({port, {:data, {:eol, line}}}, %{port: port} = state) do
    mode = Application.get_env(:conductor, :dispatch_mode, :acp)

    case mode do
      :acp ->
        # ACP JSON-RPC message from the agent
        handle_acp_message(line, state)

      :cli ->
        # CLI mode: try to extract session_id from --output-format json output
        handle_cli_output(line, state)
    end
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
    pipeline_map = Pipeline.to_map(state.pipeline)
    {done, total} = Pipeline.progress(state.pipeline)
    current = Pipeline.current_agent(state.pipeline)
    agents = Pipeline.agents(state.pipeline)

    {:reply,
     %{
       bead_id: state.pipeline.bead_id,
       repo: state.pipeline.repo,
       issue_type: state.pipeline.issue_type,
       title: state.bead_title,
       current_agent: current,
       agents: agents,
       progress: "#{done}/#{total}",
       os_pid: state.os_pid,
       session_id: state.session_id,
       acp_stop_reason: state.acp_stop_reason,
       started_at: state.started_at,
       elapsed_s: DateTime.diff(DateTime.utc_now(), state.started_at, :second),
       history:
         Enum.map(pipeline_map.history, fn h ->
           "#{h.agent}: #{h.outcome}#{if h.detail, do: " (#{h.detail})", else: ""}"
         end)
     }, state}
  end

  @impl true
  def terminate(reason, state) do
    bead_id = state.pipeline.bead_id
    {done, total} = Pipeline.progress(state.pipeline)

    Logger.info(
      "[worker] #{bead_id}: terminated (#{done}/#{total} phases, reason=#{inspect(reason)})"
    )

    # Clean up the Port if still open
    if state.port && Port.info(state.port) != nil do
      Port.close(state.port)
    end

    # Clean up workspace (best-effort — may already be cleaned up on :done)
    if reason != :normal do
      client().workspace_cleanup(bead_id, state.pipeline.repo)
    end

    :ok
  end

  # -- Phase progression --

  defp on_success(state) do
    pipeline = Pipeline.record(state.pipeline, :pass)
    bead_id = pipeline.bead_id
    agent = Pipeline.current_agent(pipeline)

    # Checkpoint workspace (jj commit + bookmark) before advancing
    case client().workspace_checkpoint(bead_id, pipeline.repo, "fix(#{bead_id}): #{agent} pass") do
      {:ok, %{"change_id" => cid}} when not is_nil(cid) ->
        Logger.info("[checkpoint] #{bead_id}: jj change #{cid}")

      {:ok, _} ->
        :ok

      {:error, reason} ->
        Logger.warning("[checkpoint] #{bead_id}: failed: #{inspect(reason)}")
    end

    case Pipeline.advance(pipeline) do
      :done ->
        Logger.info(
          "[pipeline] #{bead_id}: complete (#{Enum.join(Pipeline.agents(pipeline), " → ")})"
        )

        client().bead_comment(
          pipeline.repo,
          bead_id,
          "Pipeline complete: #{Enum.join(Pipeline.agents(pipeline), " → ")}"
        )

        # Clean up workspace on pipeline completion
        client().workspace_cleanup(bead_id, pipeline.repo)

        {:stop, :normal, %{state | pipeline: pipeline}}

      {:next, next_pipeline} ->
        next_agent = Pipeline.current_agent(next_pipeline)
        Logger.info("[pipeline] #{bead_id}: #{agent} passed → #{next_agent}")

        client().bead_comment(
          pipeline.repo,
          bead_id,
          "Phase passed: #{agent} → advancing to #{next_agent}"
        )

        case start_agent_process(next_pipeline, state) do
          {:ok, port, os_pid} ->
            step = Pipeline.current_step(next_pipeline)
            timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)
            validate_ref = schedule_validation(step)

            {:noreply,
             %{
               state
               | pipeline: next_pipeline,
                 port: port,
                 os_pid: os_pid,
                 timeout_ref: timeout_ref,
                 validate_ref: validate_ref,
                 started_at: DateTime.utc_now(),
                 acp_stop_reason: nil,
                 session_id: nil,
                 pending_tools: %{}
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

      backoff = min((30_000 * :math.pow(2, retries)) |> trunc(), 300_000)
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

  defp start_agent_process(pipeline, state) do
    case spawn_fn() do
      nil ->
        mode = Application.get_env(:conductor, :dispatch_mode, :acp)

        case mode do
          :acp -> start_acp_process(pipeline, state)
          :cli -> start_cli_process(pipeline, state)
        end

      fun when is_function(fun) ->
        fun.(pipeline)
    end
  end

  # Fallback: claude -p via Port. No ACP protocol, just fire-and-forget.
  # Uses Claude Code's built-in OAuth auth — no API key needed.
  # MCP servers available. Exit code only (no mid-execution feedback).
  # Stdout is captured via {:line, ...} to extract session_id from JSON output.
  defp start_cli_process(pipeline, state) do
    agent = Pipeline.current_agent(pipeline)
    bead_id = pipeline.bead_id
    # Use workspace work_dir if available, fall back to repo
    work_dir = (state && state.work_dir) || pipeline.repo
    prompt = build_prompt(pipeline, state)

    binary =
      case Application.get_env(:conductor, :agent_provider, "claude") do
        "gemini" -> "gemini"
        _ -> "claude"
      end

    # Session resume: if retrying and we have a previous session_id,
    # pass --resume to preserve agent context across retries
    previous_session = state && state.session_id

    base_args = ["-p", prompt, "--output-format", "json"]

    args =
      if previous_session do
        base_args ++ ["--resume", previous_session]
      else
        base_args
      end

    try do
      port =
        Port.open(
          {:spawn_executable, System.find_executable(binary)},
          [
            :binary,
            :exit_status,
            {:line, 65_536},
            args: args,
            cd: to_charlist(work_dir)
          ]
        )

      {:os_pid, os_pid} = Port.info(port, :os_pid)

      if previous_session do
        Logger.info(
          "[worker] #{bead_id}: resumed #{agent} via CLI (#{binary} -p --resume #{previous_session}, pid=#{os_pid})"
        )
      else
        Logger.info("[worker] #{bead_id}: spawned #{agent} via CLI (#{binary} -p, pid=#{os_pid})")
      end

      {:ok, port, os_pid}
    rescue
      e -> {:error, Exception.message(e)}
    end
  end

  defp start_acp_process(pipeline, state) do
    alias Conductor.AcpClient

    agent = Pipeline.current_agent(pipeline)
    bead_id = pipeline.bead_id
    # Use workspace work_dir if available, fall back to repo
    work_dir = (state && state.work_dir) || pipeline.repo
    binary = agent_binary(agent)
    prompt = build_prompt(pipeline, state)

    # Session resume: if retrying and we have a previous session_id,
    # resume instead of starting fresh (preserves agent context)
    previous_session = state && state.session_id

    try do
      {:ok, port, os_pid} = AcpClient.start(binary, work_dir)

      if previous_session do
        AcpClient.resume(port, previous_session, prompt)

        Logger.info(
          "[worker] #{bead_id}: resumed #{agent} session=#{previous_session} (pid=#{os_pid})"
        )
      else
        _sid = AcpClient.prompt(port, nil, work_dir, prompt)
        Logger.info("[worker] #{bead_id}: spawned #{agent} via ACP (#{binary}, pid=#{os_pid})")
      end

      {:ok, port, os_pid}
    rescue
      e -> {:error, Exception.message(e)}
    end
  end

  defp build_prompt(pipeline, state) do
    bead_id = pipeline.bead_id
    repo = pipeline.repo
    agent = Pipeline.current_agent(pipeline)
    title = (state && state.bead_title) || "#{pipeline.issue_type} work"
    description = (state && state.bead_description) || ""

    desc_section =
      if description != "" do
        "Description: #{description}\n\n"
      else
        ""
      end

    "Fix this issue. Make the minimal change needed.\n\n" <>
      "Bead ID: #{bead_id}\n" <>
      "Repo: #{repo}\n" <>
      "Agent: #{agent}\n" <>
      "Title: #{title}\n" <>
      desc_section <>
      "After fixing:\n" <>
      "1. Run tests via `task test`\n" <>
      "2. Create a commit with a descriptive message\n" <>
      "3. Close this bead: call mcp__rsry__rsry_bead_close with repo_path=\"#{repo}\" and id=\"#{bead_id}\"\n" <>
      "4. Report what you changed"
  end

  # -- CLI output handling --

  defp handle_cli_output(line, state) do
    # Claude CLI with --output-format json emits a JSON blob on stdout.
    # Extract session_id if present to enable --resume on retry.
    case Jason.decode(line) do
      {:ok, %{"session_id" => sid}} when is_binary(sid) and sid != "" ->
        Logger.debug("[cli] #{state.pipeline.bead_id}: captured session_id=#{sid}")
        {:noreply, %{state | session_id: sid}}

      {:ok, _json} ->
        # Valid JSON but no session_id field — ignore
        {:noreply, state}

      {:error, _} ->
        # Not JSON (stderr leak or partial line) — ignore
        {:noreply, state}
    end
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

      {:ok, {:prompt_complete, reason, _result}} ->
        # Agent signaled completion via ACP before exit.
        # Store the stop_reason — exit_status handler uses it to determine pass/fail.
        # "refusal" or "max_tokens" = fail even if exit code 0.
        {:noreply, %{state | acp_stop_reason: reason}}

      {:ok, {:session_created, sid, _result}} ->
        # Real session ID from session/new response — store for resume on retry
        Logger.debug("[acp] #{state.pipeline.bead_id}: session=#{sid}")
        {:noreply, %{state | session_id: sid}}

      {:ok, {:initialized, result}} ->
        Logger.debug("[acp] #{state.pipeline.bead_id}: agent caps=#{inspect(Map.keys(result))}")
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

  # -- Validation helpers --

  defp schedule_validation(%{validation: %{interval_ms: ms}}) do
    Process.send_after(self(), :validate, ms)
  end

  defp schedule_validation(_step), do: nil

  defp run_validation(command, work_dir) do
    case System.cmd("/bin/sh", ["-c", command], cd: work_dir, stderr_to_stdout: true) do
      {_output, 0} -> :pass
      {output, _code} -> {:fail, output}
    end
  end

  defp truncate(s, max) when byte_size(s) <= max, do: s
  defp truncate(s, max), do: String.slice(s, 0, max) <> "\n... (truncated)"

  # ACP adapter binaries for each provider.
  # Each wraps the provider's SDK to speak ACP over stdio.
  # Install: npm install -g @zed-industries/claude-agent-acp
  @provider_binaries %{
    "claude" => "claude-agent-acp",
    "gemini" => "gemini-agent-acp",
    "codex" => "codex-agent-acp",
    "copilot" => "copilot-agent-acp",
    "qwen-code" => "qwen-code-agent-acp"
  }

  defp agent_binary(_agent_name) do
    provider = Application.get_env(:conductor, :agent_provider, "claude")
    Map.get(@provider_binaries, provider, provider)
  end
end
