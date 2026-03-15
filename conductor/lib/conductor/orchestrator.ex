defmodule Conductor.Orchestrator do
  @moduledoc """
  Periodic orchestration loop — poll rsry for beads, dispatch to agents.

  Replaces Reconciler.iterate() from reconcile.rs. Uses Process.send_after
  for self-correcting intervals (next tick starts after current completes).
  """
  use GenServer
  require Logger

  alias Conductor.{AgentSupervisor, Pipeline, RsryClient}

  defstruct [:interval_ms, :max_concurrent, :timer_ref, paused: true, dispatched: MapSet.new()]

  # -- Public API --

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: __MODULE__)
  end

  @doc "Start dispatching. Orchestrator boots paused — call this to begin."
  def start_dispatching do
    GenServer.call(__MODULE__, :start_dispatching)
  end

  @doc "Pause dispatching. Running agents continue, no new dispatches."
  def pause do
    GenServer.call(__MODULE__, :pause)
  end

  @doc "Is the orchestrator actively dispatching?"
  def running? do
    GenServer.call(__MODULE__, :running?)
  catch
    :exit, _ -> false
  end

  # -- GenServer callbacks --

  @impl true
  def init(_opts) do
    interval = Application.get_env(:conductor, :scan_interval_ms, 30_000)
    max_concurrent = Application.get_env(:conductor, :max_concurrent, 3)
    auto_start = Application.get_env(:conductor, :auto_start, false)

    state = %__MODULE__{
      interval_ms: interval,
      max_concurrent: max_concurrent,
      paused: !auto_start
    }

    state =
      if auto_start && is_integer(interval) do
        Logger.info(
          "[orchestrator] started (interval=#{interval}ms, max=#{max_concurrent}, auto)"
        )

        ref = Process.send_after(self(), :tick, 1_000)
        %{state | timer_ref: ref, paused: false}
      else
        Logger.info(
          "[orchestrator] started PAUSED (call Conductor.Orchestrator.start_dispatching())"
        )

        state
      end

    {:ok, state}
  end

  @impl true
  def handle_call(:start_dispatching, _from, state) do
    if state.paused do
      Logger.info("[orchestrator] unpaused — dispatching started")
      send(self(), :tick)
      {:reply, :ok, %{state | paused: false}}
    else
      {:reply, {:already_running}, state}
    end
  end

  @impl true
  def handle_call(:pause, _from, state) do
    if state.timer_ref, do: Process.cancel_timer(state.timer_ref)
    Logger.info("[orchestrator] paused")
    {:reply, :ok, %{state | paused: true, timer_ref: nil}}
  end

  @impl true
  def handle_call(:running?, _from, state) do
    {:reply, !state.paused, state}
  end

  @impl true
  def handle_info(:tick, %{paused: true} = state) do
    {:noreply, state}
  end

  @impl true
  def handle_info(:tick, state) do
    state = do_tick(state)

    ref =
      if is_integer(state.interval_ms),
        do: Process.send_after(self(), :tick, state.interval_ms)

    {:noreply, %{state | timer_ref: ref}}
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
                    Logger.info(
                      "[orchestrator] dispatched #{bead["id"]} (#{bead["owner"] || Pipeline.default_agent(bead["issue_type"])})"
                    )

                    MapSet.put(acc, bead["id"])

                  {:error, reason} ->
                    Logger.error(
                      "[orchestrator] failed to start #{bead["id"]}: #{inspect(reason)}"
                    )

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
          |> Enum.sort_by(& &1["priority"])

        {:ok, dispatchable}

      {:ok, other} ->
        {:ok, Map.get(other, "beads", [])}

      error ->
        error
    end
  end
end
