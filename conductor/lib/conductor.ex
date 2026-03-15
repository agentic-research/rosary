defmodule Conductor do
  @moduledoc """
  OTP-based agent orchestration for rosary.

  Conductor is the control plane for rosary's agent dispatch pipeline.
  It connects to rsry over HTTP/MCP, supervises agent processes via OTP,
  and manages pipeline progression (dev → staging → prod).

  ## Quick Start

      # Start the conductor (connects to rsry on localhost:8383)
      mix run --no-halt

      # Or in IEx for debugging
      iex -S mix

  ## Architecture

  - `Conductor.RsryClient` — HTTP client to rsry's MCP endpoint
  - `Conductor.Orchestrator` — periodic poll/triage/dispatch loop
  - `Conductor.AgentSupervisor` — DynamicSupervisor, one agent per bead
  - `Conductor.AgentWorker` — GenServer per bead with Port monitoring
  - `Conductor.Pipeline` — first-class pipeline struct (the closure)

  ## Pipelines

  Pipelines define the agent phases a bead passes through:

      bug:     dev-agent → staging-agent
      feature: dev-agent → staging-agent → prod-agent
      task:    dev-agent
      review:  staging-agent
      epic:    pm-agent
  """

  alias Conductor.{AgentSupervisor, Orchestrator, Pipeline, RsryClient}

  @doc "Current status: connected?, active agents, orchestrator state."
  def status do
    %{
      connected: safe_call(fn -> RsryClient.connected?() end, false),
      active_agents: safe_call(fn -> AgentSupervisor.active_count() end, 0),
      orchestrator: safe_call(fn -> Orchestrator.running?() end, false)
    }
  end

  @doc "Start the orchestrator's dispatch loop."
  def start, do: Orchestrator.start_dispatching()

  @doc "Pause the orchestrator (running agents continue)."
  def pause, do: Orchestrator.pause()

  @doc """
  Manually dispatch a bead (bypasses orchestrator triage).

  ## Examples

      Conductor.dispatch("rsry-abc123", "/path/to/repo")
      Conductor.dispatch("rsry-abc123", "/path/to/repo", issue_type: "bug")
  """
  def dispatch(bead_id, repo_path, opts \\ []) do
    issue_type = opts[:issue_type] || "task"
    owner = opts[:agent] || Pipeline.default_agent(issue_type)

    bead = %{
      "id" => bead_id,
      "repo" => repo_path,
      "issue_type" => issue_type,
      "owner" => owner
    }

    AgentSupervisor.start_agent(bead)
  end

  @doc "List active agent workers with their pipeline state."
  def agents do
    AgentSupervisor.which_agents()
    |> Enum.flat_map(fn
      {_, pid, :worker, _} when is_pid(pid) ->
        try do
          [Conductor.AgentWorker.get_state(pid)]
        catch
          :exit, _ -> []
        end

      _ ->
        []
    end)
  end

  defp safe_call(fun, default) do
    fun.()
  catch
    :exit, _ -> default
  end
end
