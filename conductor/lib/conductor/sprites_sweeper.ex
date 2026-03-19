defmodule Conductor.SpritesSweeper do
  @moduledoc """
  Safety net for orphan sprites. Periodically checks for sprites that have
  no corresponding active AgentWorker and destroys them.

  Only started when `compute_backend: :sprites` is configured.
  Runs every 5 minutes by default.

  Uses an ETS table (`:sprites_registry`) to track created sprite names.
  SpritesClient calls should register/unregister via `track/1` and `untrack/1`.
  """
  use GenServer
  require Logger

  @sweep_interval_ms 5 * 60_000
  @table :sprites_registry

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: __MODULE__)
  end

  @doc "Register a sprite name as active."
  def track(sprite_name) do
    :ets.insert(@table, {sprite_name, DateTime.utc_now()})
    :ok
  end

  @doc "Unregister a sprite name."
  def untrack(sprite_name) do
    :ets.delete(@table, sprite_name)
    :ok
  end

  @doc "List all tracked sprite names."
  def tracked do
    :ets.tab2list(@table) |> Enum.map(fn {name, _ts} -> name end)
  end

  # -- GenServer callbacks --

  @impl true
  def init(_opts) do
    table = :ets.new(@table, [:named_table, :set, :public])
    schedule_sweep()
    {:ok, %{table: table}}
  end

  @impl true
  def handle_info(:sweep, state) do
    sweep()
    schedule_sweep()
    {:noreply, state}
  end

  def handle_info(_msg, state), do: {:noreply, state}

  # -- Internal --

  defp schedule_sweep do
    Process.send_after(self(), :sweep, @sweep_interval_ms)
  end

  defp sweep do
    tracked_names = tracked()

    if tracked_names == [] do
      :ok
    else
      # Get active worker bead IDs
      active_bead_ids =
        case Conductor.AgentSupervisor.which_children() do
          children when is_list(children) ->
            children
            |> Enum.flat_map(fn {_, pid, _, _} ->
              try do
                state = Conductor.AgentWorker.get_state(pid)
                [state.bead_id]
              rescue
                _ -> []
              catch
                :exit, _ -> []
              end
            end)

          _ ->
            []
        end

      active_sprite_names = MapSet.new(active_bead_ids, &Conductor.SpritesClient.sprite_name/1)

      sprites_client =
        Application.get_env(:conductor, :sprites_client_mod, Conductor.SpritesClient)

      orphans = Enum.reject(tracked_names, &MapSet.member?(active_sprite_names, &1))

      for name <- orphans do
        Logger.warning("[sweeper] destroying orphan sprite: #{name}")
        sprites_client.destroy_sprite(name)
        untrack(name)
      end

      if orphans != [] do
        Logger.info("[sweeper] cleaned #{length(orphans)} orphan sprite(s)")
      end
    end
  end
end
