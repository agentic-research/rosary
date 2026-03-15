defmodule Conductor.Orchestrator do
  @moduledoc """
  Periodic orchestration loop — poll rsry for beads, dispatch to agents.

  Replaces Reconciler.iterate() from reconcile.rs. Uses Process.send_after
  for self-correcting intervals (next tick starts after current completes).
  """
  use GenServer
  require Logger

  alias Conductor.{AgentSupervisor, Pipeline, RsryClient}

  defstruct [:interval_ms, :max_concurrent, dispatched: MapSet.new()]

  # -- Public API --

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: __MODULE__)
  end

  # -- GenServer callbacks --

  @impl true
  def init(_opts) do
    interval = Application.get_env(:conductor, :scan_interval_ms, 30_000)
    max_concurrent = Application.get_env(:conductor, :max_concurrent, 3)

    Logger.info("[orchestrator] started (interval=#{interval}ms, max_concurrent=#{max_concurrent})")

    # Run first tick immediately
    send(self(), :tick)

    {:ok, %__MODULE__{interval_ms: interval, max_concurrent: max_concurrent}}
  end

  @impl true
  def handle_info(:tick, state) do
    state = do_tick(state)
    Process.send_after(self(), :tick, state.interval_ms)
    {:noreply, state}
  end

  @impl true
  def handle_info(_msg, state), do: {:noreply, state}

  # -- Orchestration loop --

  defp do_tick(state) do
    active = AgentSupervisor.active_count()
    slots = max(state.max_concurrent - active, 0)

    if slots == 0 do
      Logger.debug("[orchestrator] all slots full (#{active}/#{state.max_concurrent})")
      state
    else
      case fetch_dispatchable_beads() do
        {:ok, beads} ->
          to_dispatch =
            beads
            |> Enum.reject(&MapSet.member?(state.dispatched, &1["id"]))
            |> Enum.take(slots)

          dispatched_ids =
            for bead <- to_dispatch, reduce: state.dispatched do
              acc ->
                case AgentSupervisor.start_agent(bead) do
                  {:ok, _pid} ->
                    Logger.info("[orchestrator] dispatched #{bead["id"]} (#{bead["owner"] || Pipeline.default_agent(bead["issue_type"])})")
                    MapSet.put(acc, bead["id"])

                  {:error, reason} ->
                    Logger.error("[orchestrator] failed to start #{bead["id"]}: #{inspect(reason)}")
                    acc
                end
            end

          %{state | dispatched: dispatched_ids}

        {:error, reason} ->
          Logger.error("[orchestrator] failed to fetch beads: #{inspect(reason)}")
          state
      end
    end
  end

  defp fetch_dispatchable_beads do
    case RsryClient.list_beads("open") do
      {:ok, %{"beads" => beads}} ->
        # Filter to P0-P2 beads with owners, sorted by priority
        dispatchable =
          beads
          |> Enum.filter(&(&1["priority"] <= 2))
          |> Enum.reject(&(&1["issue_type"] == "epic"))
          |> Enum.sort_by(&(&1["priority"]))

        {:ok, dispatchable}

      {:ok, other} ->
        {:ok, Map.get(other, "beads", [])}

      error ->
        error
    end
  end
end
