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
    :provider_name,
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

    # Build partial state so build_prompt can access title/description on first spawn
    init_state = %__MODULE__{
      pipeline: pipeline,
      bead_title: title,
      bead_description: description,
      work_dir: work_dir,
      started_at: DateTime.utc_now()
    }

    case start_agent_process(pipeline, init_state) do
      {:ok, port, os_pid} ->
        step = Pipeline.current_step(pipeline)
        timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)
        validate_ref = schedule_validation(step)

        # Provider identifier: string for remote (sprite name), integer for local (OS pid)
        provider_name = if is_binary(os_pid), do: os_pid, else: init_state.provider_name

        {:ok,
         %{
           init_state
           | port: port,
             os_pid: os_pid,
             timeout_ref: timeout_ref,
             validate_ref: validate_ref,
             started_at: DateTime.utc_now(),
             provider_name: provider_name
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
    if state.port, do: provider().stop_process(state.port)

    pipeline = Pipeline.record(state.pipeline, :timeout, "exceeded #{step.timeout_ms}ms")
    # Port/process close will trigger exit_status message
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

        provider_name = if is_binary(os_pid), do: os_pid, else: state.provider_name

        {:noreply,
         %{
           state
           | port: port,
             os_pid: os_pid,
             timeout_ref: timeout_ref,
             validate_ref: validate_ref,
             started_at: DateTime.utc_now(),
             acp_stop_reason: nil,
             provider_name: provider_name
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
        case run_validation(command, state.work_dir, state.provider_name) do
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

                if state.port, do: provider().stop_process(state.port)
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

    # Clean up the agent process if still open
    if state.port && provider().alive?(state.port) do
      provider().stop_process(state.port)
    end

    # Clean up workspace (best-effort — may already be cleaned up on :done)
    if reason != :normal do
      client().workspace_cleanup(bead_id, state.pipeline.repo)
    end

    # Deprovision compute environment on terminal exit (success or deadletter)
    if state.provider_name && reason == :normal do
      provider().deprovision(state.provider_name)
      Logger.info("[worker] #{bead_id}: deprovisioned #{state.provider_name}")
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

    # Write handoff file for the phase that just completed
    write_handoff(state, pipeline, agent)

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

        # Terminal step: merge or PR based on issue type
        merge_or_pr(state, pipeline)

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

            provider_name = if is_binary(os_pid), do: os_pid, else: state.provider_name

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
                 pending_tools: %{},
                 provider_name: provider_name
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

  # -- Agent process spawning (via Provider abstraction) --

  defp start_agent_process(pipeline, state) do
    case spawn_fn() do
      nil ->
        mode = Application.get_env(:conductor, :dispatch_mode, :acp)
        prov = provider()
        name = provider_name(pipeline.bead_id)

        # Provision compute environment (no-op for local, creates VM for remote)
        is_retry = state && state.provider_name == name

        provision_result =
          if is_retry do
            Logger.info("[worker] #{pipeline.bead_id}: reusing provider #{name} for retry")
            :ok
          else
            prov.provision(name, pipeline.repo, %{})
          end

        case provision_result do
          :ok ->
            {binary, args, work_dir} = build_agent_command(pipeline, state, mode)

            case prov.spawn_process(name, binary, args, work_dir, %{}, self()) do
              {:ok, handle, id} ->
                # For ACP, send the initialize + prompt after spawn
                if mode == :acp do
                  send_acp_init(handle, pipeline, state)
                end

                Logger.info(
                  "[worker] #{pipeline.bead_id}: spawned #{Pipeline.current_agent(pipeline)} " <>
                    "via #{mode} on #{inspect(prov)} (id=#{id})"
                )

                {:ok, handle, id}

              {:error, reason} ->
                unless is_retry, do: prov.deprovision(name)
                {:error, reason}
            end

          {:error, reason} ->
            prov.deprovision(name)
            {:error, reason}
        end

      fun when is_function(fun) ->
        fun.(pipeline)
    end
  end

  # Build command + args based on protocol mode.
  # CLI mode: claude -p with --allowedTools and --output-format json
  # ACP mode: agent binary with ACP args (JSON-RPC protocol via stdin/stdout)
  defp build_agent_command(pipeline, state, :cli) do
    binary =
      case Application.get_env(:conductor, :agent_provider, "claude") do
        "gemini" -> "gemini"
        _ -> "claude"
      end

    executable = System.find_executable(binary) || binary
    prompt = build_prompt(pipeline, state)
    previous_session = state && state.session_id

    allowed_tools =
      case pipeline.issue_type do
        t when t in ["review", "survey", "audit"] ->
          "Read,Glob,Grep,mcp__mache__*,mcp__rsry__*"

        t when t in ["epic", "plan", "triage"] ->
          "Read,Glob,Grep,mcp__mache__*,mcp__rsry__*"

        _ ->
          "Read,Edit,Write,Bash(cargo *),Bash(go *),Bash(git *),Bash(task *),Glob,Grep,mcp__mache__*,mcp__rsry__*"
      end

    base_args = ["-p", prompt, "--allowedTools", allowed_tools, "--output-format", "json"]
    args = if previous_session, do: base_args ++ ["--resume", previous_session], else: base_args
    work_dir = (state && state.work_dir) || pipeline.repo

    {executable, args, work_dir}
  end

  defp build_agent_command(pipeline, state, :acp) do
    binary = agent_binary(Pipeline.current_agent(pipeline))
    executable = System.find_executable(binary) || binary
    args = Conductor.AcpClient.acp_args(binary)
    work_dir = (state && state.work_dir) || pipeline.repo

    {executable, args, work_dir}
  end

  # Send ACP initialize + session/new + prompt after process is spawned.
  # This was previously done inside AcpClient.start/2, but now the provider
  # handles spawning and we handle protocol separately.
  defp send_acp_init(handle, pipeline, state) do
    alias Conductor.AcpClient

    work_dir = (state && state.work_dir) || pipeline.repo
    prompt = build_prompt(pipeline, state)
    previous_session = state && state.session_id

    # Initialize ACP protocol
    AcpClient.send_initialize(handle)

    if previous_session do
      AcpClient.resume(handle, previous_session, prompt)
    else
      AcpClient.prompt(handle, nil, work_dir, prompt)
    end
  end

  defp build_prompt(pipeline, state) do
    bead_id = pipeline.bead_id
    repo = pipeline.repo
    agent = Pipeline.current_agent(pipeline)
    title = (state && state.bead_title) || "#{pipeline.issue_type} work"
    description = (state && state.bead_description) || ""
    work_dir = (state && state.work_dir) || repo

    desc_section =
      if description != "" do
        "Description: #{description}\n\n"
      else
        ""
      end

    # Read handoff chain from previous phases (if any)
    handoff_section = read_handoff_chain(work_dir, pipeline.current)

    "Fix this issue. Make the minimal change needed.\n\n" <>
      "Bead ID: #{bead_id}\n" <>
      "Repo: #{repo}\n" <>
      "Agent: #{agent}\n" <>
      "Title: #{title}\n" <>
      desc_section <>
      handoff_section <>
      "After fixing:\n" <>
      "1. Run tests via `task test`\n" <>
      "2. Commit your changes (git add + git commit with bead:#{bead_id} in message)\n" <>
      "3. Close this bead: call mcp__rsry__rsry_bead_close with repo_path=\"#{repo}\" and id=\"#{bead_id}\"\n" <>
      "4. Report what you changed"
  end

  defp merge_or_pr(state, pipeline) do
    _work_dir = state.work_dir || pipeline.repo
    repo = pipeline.repo
    bead_id = pipeline.bead_id
    branch = "fix/#{bead_id}"
    needs_pr = pipeline.issue_type in ["feature", "epic"]

    if needs_pr do
      # Push branch — PR creation handled separately (github.rs or manual)
      case System.cmd("git", ["push", "origin", branch], cd: repo, stderr_to_stdout: true) do
        {_, 0} ->
          Logger.info("[terminal] #{bead_id}: pushed #{branch} — PR needed")

        {err, _} ->
          Logger.warning("[terminal] #{bead_id}: push failed: #{err}")
      end
    else
      # Fast-forward merge for small beads (chore, task, bug)
      case System.cmd("git", ["merge", "--ff-only", branch], cd: repo, stderr_to_stdout: true) do
        {_, 0} ->
          Logger.info("[terminal] #{bead_id}: ff-merged #{branch} to main")
          System.cmd("git", ["push", "origin", "main"], cd: repo, stderr_to_stdout: true)

        {err, _} ->
          Logger.warning("[terminal] #{bead_id}: ff-merge failed (#{err}), pushing branch")
          System.cmd("git", ["push", "origin", branch], cd: repo, stderr_to_stdout: true)
      end
    end
  end

  defp write_handoff(state, pipeline, agent) do
    work_dir = state.work_dir || pipeline.repo
    phase = pipeline.current
    bead_id = pipeline.bead_id

    handoff = %{
      schema_version: "1",
      phase: phase,
      from_agent: agent,
      to_agent: Pipeline.current_agent(pipeline),
      bead_id: bead_id,
      provider: Application.get_env(:conductor, :agent_provider, "claude"),
      summary: "Phase #{phase} (#{agent}) completed",
      files_changed: [],
      lines_changed: %{added: 0, removed: 0},
      review_hints: [],
      artifacts: %{
        manifest: ".rsry-dispatch.json",
        log: ".rsry-stream-#{phase}.jsonl",
        previous_handoff: if(phase > 0, do: ".rsry-handoff-#{phase - 1}.json")
      },
      verdict: nil,
      timestamp: DateTime.utc_now() |> DateTime.to_iso8601()
    }

    path = Path.join(work_dir, ".rsry-handoff-#{phase}.json")

    case Jason.encode(handoff, pretty: true) do
      {:ok, json} ->
        File.write(path, json)
        Logger.info("[handoff] #{bead_id}: wrote #{path}")

      {:error, reason} ->
        Logger.warning("[handoff] #{bead_id}: failed to encode: #{inspect(reason)}")
    end
  end

  defp read_handoff_chain(work_dir, current_phase) when current_phase > 0 do
    handoffs =
      0..(current_phase - 1)
      |> Enum.map(fn phase ->
        path = Path.join(work_dir, ".rsry-handoff-#{phase}.json")

        case File.read(path) do
          {:ok, content} ->
            case Jason.decode(content) do
              {:ok, h} -> h
              _ -> nil
            end

          _ ->
            nil
        end
      end)
      |> Enum.reject(&is_nil/1)

    if handoffs == [] do
      ""
    else
      sections =
        Enum.map(handoffs, fn h ->
          "### Phase #{h["phase"]} (#{h["from_agent"]} via #{h["provider"]})\n" <>
            "Summary: #{h["summary"]}\n" <>
            "Files: #{Enum.join(h["files_changed"] || [], ", ")}\n" <>
            if(h["review_hints"] && h["review_hints"] != [],
              do:
                "Review hints:\n" <>
                  Enum.map_join(h["review_hints"], "\n", &"- #{&1}") <> "\n",
              else: ""
            )
        end)

      "\n## Previous Phase Context\n\n" <>
        Enum.join(sections, "\n") <>
        "\nHandoff files are in your working directory. Use mache MCP tools to structurally review the changes.\n\n"
    end
  end

  defp read_handoff_chain(_work_dir, _phase), do: ""

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

  defp run_validation(command, work_dir, prov_name) do
    case provider().exec_sync(prov_name || "local", command, work_dir) do
      {:ok, {_output, 0}} -> :pass
      {:ok, {output, _code}} -> {:fail, output}
      {:error, reason} -> {:fail, inspect(reason)}
    end
  end

  defp truncate(s, max) when byte_size(s) <= max, do: s
  defp truncate(s, max), do: String.slice(s, 0, max) <> "\n... (truncated)"

  # -- Provider helpers --

  defp provider, do: Conductor.Provider.module()

  defp provider_name(bead_id) do
    case Application.get_env(:conductor, :compute_backend, :local) do
      :sprites -> Conductor.SpritesClient.sprite_name(bead_id)
      :local -> bead_id
    end
  end

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
