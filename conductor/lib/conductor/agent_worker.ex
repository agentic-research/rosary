defmodule Conductor.AgentWorker do
  @moduledoc """
  GenServer managing a bead's full pipeline execution.

  Owns the OS process via an Erlang Port. Gets instant notification
  via `{port, {:exit_status, code}}` — no polling.

  The pipeline (closure) is the state. Phase advancement is synchronous
  within message handlers — no window for lost continuations.
  """
  use GenServer, restart: :temporary
  require Logger

  alias Conductor.{Pipeline, RsryClient}

  defstruct [
    :pipeline,
    :port,
    :os_pid,
    :timeout_ref,
    :started_at
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

    # Build the pipeline — the closure that carries the full execution plan
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

    case dispatch_current(pipeline) do
      {:ok, os_pid} ->
        step = Pipeline.current_step(pipeline)
        timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)
        port = open_pid_monitor(os_pid)

        {:ok,
         %__MODULE__{
           pipeline: pipeline,
           port: port,
           os_pid: os_pid,
           timeout_ref: timeout_ref,
           started_at: DateTime.utc_now()
         }}

      {:error, reason} ->
        Logger.error("[worker] #{bead_id}: dispatch failed: #{inspect(reason)}")
        {:stop, {:dispatch_failed, reason}}
    end
  end

  # -- Agent process exited --

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
    Logger.warning("[timeout] #{bead_id}: killing #{agent} (pid=#{state.os_pid})")

    if state.os_pid, do: System.cmd("kill", [to_string(state.os_pid)], stderr_to_stdout: true)

    # Record timeout, then the exit_status handler will fire
    pipeline = Pipeline.record(state.pipeline, :timeout, "exceeded #{Pipeline.current_step(state.pipeline).timeout_ms}ms")
    {:noreply, %{state | pipeline: pipeline}}
  end

  @impl true
  def handle_info(_msg, state), do: {:noreply, state}

  @impl true
  def handle_call(:get_state, _from, state) do
    info = %{
      bead_id: state.pipeline.bead_id,
      pipeline: Pipeline.to_map(state.pipeline),
      progress: Pipeline.progress(state.pipeline),
      os_pid: state.os_pid,
      started_at: state.started_at
    }

    {:reply, info, state}
  end

  @impl true
  def terminate(reason, state) do
    bead_id = state.pipeline.bead_id
    {done, total} = Pipeline.progress(state.pipeline)
    Logger.info("[worker] #{bead_id}: terminated (#{done}/#{total} phases, reason=#{inspect(reason)})")
    :ok
  end

  # -- Phase progression: the closure in action --

  defp on_success(state) do
    pipeline = Pipeline.record(state.pipeline, :pass)
    bead_id = pipeline.bead_id
    agent = Pipeline.current_agent(pipeline)

    case Pipeline.advance(pipeline) do
      :done ->
        # Pipeline complete — all phases passed
        Logger.info("[pipeline] #{bead_id}: complete (#{Enum.join(Pipeline.agents(pipeline), " → ")})")

        RsryClient.bead_comment(
          pipeline.repo,
          bead_id,
          "Pipeline complete: #{Enum.join(Pipeline.agents(pipeline), " → ")}"
        )

        {:stop, :normal, %{state | pipeline: pipeline}}

      {:next, next_pipeline} ->
        # Advance to next agent
        next_agent = Pipeline.current_agent(next_pipeline)
        Logger.info("[pipeline] #{bead_id}: #{agent} passed → #{next_agent}")

        RsryClient.bead_comment(
          pipeline.repo,
          bead_id,
          "Phase passed: #{agent} → advancing to #{next_agent}"
        )

        case dispatch_current(next_pipeline) do
          {:ok, os_pid} ->
            step = Pipeline.current_step(next_pipeline)
            timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)
            port = open_pid_monitor(os_pid)

            {:noreply,
             %{state |
               pipeline: next_pipeline,
               port: port,
               os_pid: os_pid,
               timeout_ref: timeout_ref,
               started_at: DateTime.utc_now()
             }}

          {:error, reason} ->
            Logger.error("[pipeline] #{bead_id}: dispatch of #{next_agent} failed: #{inspect(reason)}")
            {:stop, {:phase_dispatch_failed, reason}, %{state | pipeline: next_pipeline}}
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

      RsryClient.bead_comment(
        pipeline.repo,
        bead_id,
        "#{agent} failed (exit #{exit_code}), retrying (#{retries}/#{step.max_retries})"
      )

      # Exponential backoff: 30s * 2^retries
      backoff = min(30_000 * :math.pow(2, retries) |> trunc(), 300_000)
      Process.send_after(self(), :retry, backoff)
      {:noreply, %{state | pipeline: pipeline, port: nil, os_pid: nil, timeout_ref: nil}}
    else
      Logger.warning("[deadletter] #{bead_id}: #{agent} exhausted retries")

      RsryClient.bead_comment(
        pipeline.repo,
        bead_id,
        "#{agent} exhausted retries — deadlettered"
      )

      {:stop, :normal, %{state | pipeline: pipeline}}
    end
  end

  @impl true
  def handle_info(:retry, state) do
    case dispatch_current(state.pipeline) do
      {:ok, os_pid} ->
        step = Pipeline.current_step(state.pipeline)
        timeout_ref = Process.send_after(self(), :timeout, step.timeout_ms)
        port = open_pid_monitor(os_pid)

        {:noreply,
         %{state |
           port: port,
           os_pid: os_pid,
           timeout_ref: timeout_ref,
           started_at: DateTime.utc_now()
         }}

      {:error, reason} ->
        Logger.error("[retry] #{state.pipeline.bead_id}: dispatch failed: #{inspect(reason)}")
        {:stop, {:retry_dispatch_failed, reason}, state}
    end
  end

  # -- Helpers --

  defp dispatch_current(pipeline) do
    agent = Pipeline.current_agent(pipeline)

    case RsryClient.dispatch(pipeline.bead_id, pipeline.repo, %{agent: agent}) do
      {:ok, %{"pid" => pid}} -> {:ok, pid}
      {:ok, result} -> {:ok, result["pid"]}
      {:error, _} = err -> err
    end
  end

  defp open_pid_monitor(nil), do: nil

  defp open_pid_monitor(pid) do
    Port.open(
      {:spawn_executable, "/bin/sh"},
      [
        :exit_status,
        :binary,
        args: ["-c", "while kill -0 #{pid} 2>/dev/null; do sleep 2; done; wait #{pid} 2>/dev/null"]
      ]
    )
  end
end
