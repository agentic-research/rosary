defmodule Conductor.AgentWorker do
  @moduledoc """
  GenServer managing a single agent dispatch for a bead.

  Owns the OS process via an Erlang Port. Gets instant `:DOWN`-style
  notification via `{port, {:exit_status, code}}` — no polling.

  Handles phase advancement synchronously within the message handler,
  so there's no window where a continuation can be lost.
  """
  use GenServer, restart: :temporary
  require Logger

  alias Conductor.{Pipeline, RsryClient}

  defstruct [
    :bead_id,
    :repo,
    :issue_type,
    :pipeline,
    :current_phase,
    :port,
    :os_pid,
    :timeout_ref,
    :started_at
  ]

  # -- Public API --

  def start_link(bead) do
    GenServer.start_link(__MODULE__, bead)
  end

  # -- GenServer callbacks --

  @impl true
  def init(bead) do
    bead_id = bead["id"] || bead[:id]
    repo = bead["repo"] || bead[:repo]
    issue_type = bead["issue_type"] || bead[:issue_type] || "task"
    owner = bead["owner"] || bead[:owner] || Pipeline.default_agent(issue_type)
    pipeline = Pipeline.pipeline(issue_type)
    current_phase = Enum.find_index(pipeline, &(&1 == owner)) || 0

    Logger.info("[agent] starting #{bead_id} (#{owner}, phase #{current_phase + 1}/#{length(pipeline)})")

    # Dispatch via rsry MCP — this creates workspace + spawns agent
    case dispatch(bead_id, repo, owner) do
      {:ok, result} ->
        os_pid = result["pid"]
        timeout_ms = Application.get_env(:conductor, :agent_timeout_ms, 600_000)
        timeout_ref = Process.send_after(self(), :timeout, timeout_ms)

        # Monitor the OS process via Port
        port = open_pid_monitor(os_pid)

        {:ok,
         %__MODULE__{
           bead_id: bead_id,
           repo: repo,
           issue_type: issue_type,
           pipeline: pipeline,
           current_phase: current_phase,
           port: port,
           os_pid: os_pid,
           timeout_ref: timeout_ref,
           started_at: DateTime.utc_now()
         }}

      {:error, reason} ->
        Logger.error("[agent] dispatch failed for #{bead_id}: #{inspect(reason)}")
        {:stop, {:dispatch_failed, reason}}
    end
  end

  # Agent process exited
  @impl true
  def handle_info({port, {:exit_status, code}}, %{port: port} = state) do
    Process.cancel_timer(state.timeout_ref)
    elapsed = DateTime.diff(DateTime.utc_now(), state.started_at, :second)
    current_agent = Enum.at(state.pipeline, state.current_phase)

    Logger.info(
      "[agent] #{state.bead_id} exited (code=#{code}, agent=#{current_agent}, elapsed=#{elapsed}s)"
    )

    if code == 0 do
      handle_success(state)
    else
      handle_failure(state, code)
    end
  end

  # Timeout — kill the agent
  @impl true
  def handle_info(:timeout, state) do
    Logger.warning("[timeout] killing #{state.bead_id} (pid=#{state.os_pid})")

    if state.os_pid do
      System.cmd("kill", [to_string(state.os_pid)], stderr_to_stdout: true)
    end

    # The port exit_status message will arrive and trigger handle_info
    {:noreply, state}
  end

  @impl true
  def handle_info(_msg, state), do: {:noreply, state}

  @impl true
  def terminate(reason, state) do
    Logger.info("[agent] #{state.bead_id} worker terminated: #{inspect(reason)}")
    :ok
  end

  # -- Phase progression --

  defp handle_success(state) do
    current_agent = Enum.at(state.pipeline, state.current_phase)

    case Pipeline.next_agent(state.issue_type, current_agent) do
      nil ->
        # Pipeline complete — bead stays closed (agent closed it) or we close it
        Logger.info("[phase] #{state.bead_id} pipeline complete (#{current_agent} was final)")
        RsryClient.bead_comment(state.repo, state.bead_id, "Pipeline complete: #{Enum.join(state.pipeline, " → ")}")
        {:stop, :normal, state}

      next_agent ->
        # Advance to next phase
        Logger.info("[phase] #{state.bead_id} → #{next_agent}")
        RsryClient.bead_comment(state.repo, state.bead_id, "Phase complete: #{current_agent} → #{next_agent}")

        # Dispatch next phase
        case dispatch(state.bead_id, state.repo, next_agent) do
          {:ok, result} ->
            os_pid = result["pid"]
            timeout_ms = Application.get_env(:conductor, :agent_timeout_ms, 600_000)
            timeout_ref = Process.send_after(self(), :timeout, timeout_ms)
            port = open_pid_monitor(os_pid)

            {:noreply,
             %{state |
               current_phase: state.current_phase + 1,
               port: port,
               os_pid: os_pid,
               timeout_ref: timeout_ref,
               started_at: DateTime.utc_now()
             }}

          {:error, reason} ->
            Logger.error("[phase] failed to dispatch next phase for #{state.bead_id}: #{inspect(reason)}")
            {:stop, {:phase_advance_failed, reason}, state}
        end
    end
  end

  defp handle_failure(state, exit_code) do
    Logger.warning("[agent] #{state.bead_id} failed (exit=#{exit_code})")
    RsryClient.bead_comment(state.repo, state.bead_id, "Agent exited with code #{exit_code}")
    {:stop, :normal, state}
  end

  # -- Helpers --

  defp dispatch(bead_id, repo, agent) do
    RsryClient.dispatch(bead_id, repo, %{agent: agent})
  end

  defp open_pid_monitor(nil), do: nil

  defp open_pid_monitor(pid) do
    # Use a shell command that waits for the PID to exit.
    # The Port gives us {:exit_status, code} when it does.
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
