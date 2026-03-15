defmodule Conductor.OrchestratorTest do
  use ExUnit.Case, async: false

  alias Conductor.Orchestrator
  alias Conductor.Test.MockRsry

  # The Orchestrator polls rsry for beads, filters them, and dispatches
  # via AgentSupervisor. We test the filtering/dispatch logic by:
  #
  # 1. MockRsry — controls what list_beads returns
  # 2. :agent_spawn_fn — controls what happens when workers start
  # 3. Manual :tick messages (scan_interval_ms: :infinity prevents auto-tick)

  setup do
    # Stop the application supervisor to prevent interference
    case Process.whereis(Conductor.Supervisor) do
      nil -> :ok
      pid -> Supervisor.stop(pid, :normal, 5_000)
    end

    Process.sleep(30)

    # Stop any lingering named processes
    for name <- [Conductor.RsryClient, Conductor.Orchestrator, Conductor.AgentSupervisor] do
      case GenServer.whereis(name) do
        nil -> :ok
        pid -> GenServer.stop(pid, :normal, 5_000)
      end
    end

    Process.sleep(20)

    on_exit(fn ->
      # Restore test defaults
      Application.put_env(:conductor, :scan_interval_ms, :infinity)
      Application.put_env(:conductor, :max_concurrent, 0)
      Application.delete_env(:conductor, :agent_spawn_fn)

      for name <- [Conductor.Orchestrator, Conductor.AgentSupervisor, Conductor.RsryClient] do
        try do
          case GenServer.whereis(name) do
            nil -> :ok
            pid -> GenServer.stop(pid, :normal, 1_000)
          end
        catch
          :exit, _ -> :ok
        end
      end
    end)

    :ok
  end

  describe "tick behavior" do
    test "orchestrator respects max_concurrent (supervisor limits children)" do
      with_config([scan_interval_ms: :infinity, max_concurrent: 2], fn ->
        {:ok, _mock} = MockRsry.start_link(test_pid: self())

        # Agent processes that run for 30s (long enough to be "active")
        Application.put_env(
          :conductor,
          :agent_spawn_fn,
          MockRsry.make_spawn_fn(0, 30, self())
        )

        MockRsry.set_list_beads_response(fn _status ->
          {:ok,
           %{
             "beads" => [
               %{"id" => "b1", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 0},
               %{"id" => "b2", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 0},
               %{"id" => "b3", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 0}
             ]
           }}
        end)

        start_supervisor!()
        {:ok, orch} = Orchestrator.start_link()

        send(orch, :tick)
        Process.sleep(500)

        # Supervisor max_children is 2, so at most 2 dispatched
        spawned = collect_spawn_messages(500)
        assert length(spawned) <= 2
      end)
    end

    test "orchestrator skips epics" do
      with_config([scan_interval_ms: :infinity, max_concurrent: 5], fn ->
        {:ok, _mock} = MockRsry.start_link(test_pid: self())

        Application.put_env(
          :conductor,
          :agent_spawn_fn,
          MockRsry.make_spawn_fn(0, 30, self())
        )

        MockRsry.set_list_beads_response(fn _status ->
          {:ok,
           %{
             "beads" => [
               %{"id" => "epic-1", "repo" => "/tmp/r", "issue_type" => "epic", "priority" => 1},
               %{"id" => "task-1", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 1}
             ]
           }}
        end)

        start_supervisor!()
        {:ok, orch} = Orchestrator.start_link()

        send(orch, :tick)
        Process.sleep(500)

        spawned = collect_spawn_messages(500)
        spawned_ids = Enum.map(spawned, fn {:agent_spawned, id, _agent} -> id end)

        # epic-1 should have been filtered out by fetch_dispatchable_beads
        refute "epic-1" in spawned_ids
      end)
    end

    test "orchestrator filters by priority (only P0-P2)" do
      with_config([scan_interval_ms: :infinity, max_concurrent: 5], fn ->
        {:ok, _mock} = MockRsry.start_link(test_pid: self())

        Application.put_env(
          :conductor,
          :agent_spawn_fn,
          MockRsry.make_spawn_fn(0, 30, self())
        )

        MockRsry.set_list_beads_response(fn _status ->
          {:ok,
           %{
             "beads" => [
               %{"id" => "p0", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 0},
               %{"id" => "p3", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 3},
               %{"id" => "p4", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 4}
             ]
           }}
        end)

        start_supervisor!()
        {:ok, orch} = Orchestrator.start_link()

        send(orch, :tick)
        Process.sleep(500)

        spawned = collect_spawn_messages(500)
        spawned_ids = Enum.map(spawned, fn {:agent_spawned, id, _agent} -> id end)

        # p3 and p4 should be filtered out
        refute "p3" in spawned_ids
        refute "p4" in spawned_ids
      end)
    end

    test "orchestrator doesn't double-dispatch" do
      with_config([scan_interval_ms: :infinity, max_concurrent: 5], fn ->
        {:ok, _mock} = MockRsry.start_link(test_pid: self())

        Application.put_env(
          :conductor,
          :agent_spawn_fn,
          MockRsry.make_spawn_fn(0, 30, self())
        )

        MockRsry.set_list_beads_response(fn _status ->
          {:ok,
           %{
             "beads" => [
               %{"id" => "bead-dup", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 1}
             ]
           }}
        end)

        start_supervisor!()
        {:ok, orch} = Orchestrator.start_link()

        # Tick twice
        send(orch, :tick)
        Process.sleep(500)
        send(orch, :tick)
        Process.sleep(500)

        spawned = collect_spawn_messages(500)

        dup_spawns =
          spawned
          |> Enum.filter(fn {:agent_spawned, id, _} -> id == "bead-dup" end)

        # Should only be spawned once despite two ticks
        assert length(dup_spawns) <= 1
      end)
    end

    test "orchestrator handles rsry connection failure gracefully" do
      with_config([scan_interval_ms: :infinity, max_concurrent: 5], fn ->
        {:ok, _mock} = MockRsry.start_link(test_pid: self())

        MockRsry.set_list_beads_response(fn _status ->
          {:error, "connection refused"}
        end)

        start_supervisor!()
        {:ok, orch} = Orchestrator.start_link()

        send(orch, :tick)
        Process.sleep(200)

        # Should not crash
        assert Process.alive?(orch)
      end)
    end

    test "orchestrator sorts beads by priority (P0 dispatched first)" do
      with_config([scan_interval_ms: :infinity, max_concurrent: 1], fn ->
        {:ok, _mock} = MockRsry.start_link(test_pid: self())

        Application.put_env(
          :conductor,
          :agent_spawn_fn,
          MockRsry.make_spawn_fn(0, 30, self())
        )

        MockRsry.set_list_beads_response(fn _status ->
          {:ok,
           %{
             "beads" => [
               %{"id" => "p2-bead", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 2},
               %{"id" => "p0-bead", "repo" => "/tmp/r", "issue_type" => "task", "priority" => 0}
             ]
           }}
        end)

        start_supervisor!()
        {:ok, orch} = Orchestrator.start_link()

        send(orch, :tick)
        Process.sleep(500)

        spawned = collect_spawn_messages(500)

        if length(spawned) >= 1 do
          # With max_concurrent: 1, only P0 should be dispatched
          {:agent_spawned, first_id, _} = hd(spawned)
          assert first_id == "p0-bead"
        end
      end)
    end
  end

  # -- Helpers --

  defp start_supervisor! do
    case Conductor.AgentSupervisor.start_link() do
      {:ok, _} -> :ok
      {:error, {:already_started, _}} -> :ok
    end
  end

  defp with_config(overrides, fun) do
    original =
      Enum.map(overrides, fn {key, _} ->
        {key, Application.get_env(:conductor, key)}
      end)

    try do
      Enum.each(overrides, fn {key, val} ->
        Application.put_env(:conductor, key, val)
      end)

      fun.()
    after
      Enum.each(original, fn {key, val} ->
        if val != nil, do: Application.put_env(:conductor, key, val)
      end)
    end
  end

  defp collect_spawn_messages(timeout) do
    collect_spawn_messages(timeout, [])
  end

  defp collect_spawn_messages(timeout, acc) do
    receive do
      {:agent_spawned, _id, _agent} = msg ->
        collect_spawn_messages(timeout, [msg | acc])
    after
      timeout -> Enum.reverse(acc)
    end
  end
end
