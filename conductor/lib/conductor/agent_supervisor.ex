defmodule Conductor.AgentSupervisor do
  @moduledoc """
  DynamicSupervisor for agent worker processes.

  Each dispatched bead gets its own AgentWorker under this supervisor.
  strategy: :one_for_one — agent crashes are independent.
  restart: :temporary — we don't auto-restart; the Orchestrator decides.
  """
  use DynamicSupervisor

  def start_link(opts \\ []) do
    DynamicSupervisor.start_link(__MODULE__, opts, name: __MODULE__)
  end

  @impl true
  def init(_opts) do
    DynamicSupervisor.init(
      strategy: :one_for_one,
      max_children: Application.get_env(:conductor, :max_concurrent, 5)
    )
  end

  @doc "Start an agent worker for a bead."
  def start_agent(bead) do
    spec = {Conductor.AgentWorker, bead}
    DynamicSupervisor.start_child(__MODULE__, spec)
  end

  @doc "Count of currently supervised agents."
  def active_count do
    %{active: count} = DynamicSupervisor.count_children(__MODULE__)
    count
  end

  @doc "List all supervised agent PIDs."
  def which_agents do
    DynamicSupervisor.which_children(__MODULE__)
  end
end
