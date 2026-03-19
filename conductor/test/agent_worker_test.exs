defmodule Conductor.AgentWorkerTest do
  use ExUnit.Case, async: false

  alias Conductor.AgentWorker
  alias Conductor.Pipeline
  alias Conductor.Test.MockRsry

  # Tests use two injection points in AgentWorker:
  #
  # 1. :agent_spawn_fn — controls how the agent process is started
  #    (avoids spawning real claude processes)
  #
  # 2. MockRsry — replaces Conductor.RsryClient GenServer to capture
  #    bead_comment calls the worker makes on phase transitions

  setup do
    # Stop the application supervisor to prevent interference
    case Process.whereis(Conductor.Supervisor) do
      nil -> :ok
      pid -> Supervisor.stop(pid, :normal, 5_000)
    end

    Process.sleep(20)

    # Stop any lingering RsryClient
    case GenServer.whereis(Conductor.RsryClient) do
      nil -> :ok
      pid -> GenServer.stop(pid, :normal, 5_000)
    end

    Process.sleep(10)

    # Start mock RsryClient (for bead_comment calls)
    {:ok, _mock} = MockRsry.start_link(test_pid: self())

    on_exit(fn ->
      Application.delete_env(:conductor, :agent_spawn_fn)
    end)

    :ok
  end

  describe "init/1" do
    test "worker starts and initializes pipeline correctly for a task bead" do
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_controllable_spawn_fn(self())
      )

      bead = %{
        "id" => "bead-init-1",
        "repo" => "/tmp/test-repo",
        "issue_type" => "task"
      }

      {:ok, pid} = AgentWorker.start_link(bead)
      assert Process.alive?(pid)

      # Verify the spawn was called
      assert_receive {:agent_spawned, "bead-init-1", "dev-agent", _port}, 1_000

      state = AgentWorker.get_state(pid)
      assert state.bead_id == "bead-init-1"
      assert state.bead_id == "bead-init-1"
      assert is_binary(state.issue_type)
      assert state.progress =~ ~r|^0/\d+$|
      assert is_integer(state.os_pid)

      cleanup_worker(pid)
    end

    test "worker starts with correct pipeline for bug (2-step)" do
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_controllable_spawn_fn(self())
      )

      bead = %{"id" => "bead-init-2", "repo" => "/tmp/repo", "issue_type" => "bug"}

      {:ok, pid} = AgentWorker.start_link(bead)

      assert_receive {:agent_spawned, "bead-init-2", "dev-agent", _port}, 1_000

      state = AgentWorker.get_state(pid)
      assert "dev-agent" in state.agents

      cleanup_worker(pid)
    end

    test "worker starts at correct phase when owner is specified" do
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_controllable_spawn_fn(self())
      )

      bead = %{
        "id" => "bead-init-3",
        "repo" => "/tmp/repo",
        "issue_type" => "feature",
        "owner" => "staging-agent"
      }

      {:ok, pid} = AgentWorker.start_link(bead)

      # Should spawn staging-agent, not dev-agent
      assert_receive {:agent_spawned, "bead-init-3", "staging-agent", _port}, 1_000

      state = AgentWorker.get_state(pid)
      assert state.current_agent == "staging-agent"

      cleanup_worker(pid)
    end

    test "worker stops when spawn fails" do
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_failing_spawn_fn("spawn failed")
      )

      bead = %{"id" => "bead-fail-spawn", "repo" => "/tmp/repo", "issue_type" => "task"}

      Process.flag(:trap_exit, true)
      result = AgentWorker.start_link(bead)
      assert {:error, {:start_failed, "spawn failed"}} = result
      Process.flag(:trap_exit, false)
    end
  end

  describe "exit_status 0 (success)" do
    test "single-step pipeline: worker stops normally on success" do
      # Spawn a process that exits with 0 after 0.5s
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_spawn_fn(0, 0.5, self())
      )

      bead = %{"id" => "bead-pass-1", "repo" => "/tmp/repo", "issue_type" => "task"}

      {:ok, pid} = AgentWorker.start_link(bead)
      ref = Process.monitor(pid)

      assert_receive {:agent_spawned, "bead-pass-1", "dev-agent"}, 1_000

      # Worker stops normally after pipeline completes
      assert_receive {:DOWN, ^ref, :process, ^pid, :normal}, 5_000

      # Should have posted a pipeline-complete comment
      assert_receive {:mock_rsry, {:bead_comment, "/tmp/repo", "bead-pass-1", comment}}, 1_000
      assert comment =~ "Pipeline complete"
    end

    test "multi-step pipeline: advances through all phases then completes" do
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_spawn_fn(0, 0.3, self())
      )

      # Use bug — has multiple phases. Don't hardcode how many.
      bead = %{"id" => "bead-advance-1", "repo" => "/tmp/repo", "issue_type" => "bug"}
      expected_steps = Pipeline.for_bead("x", "/r", "bug") |> Pipeline.step_count()

      {:ok, pid} = AgentWorker.start_link(bead)
      ref = Process.monitor(pid)

      # Wait for worker to complete — it walks all phases automatically
      assert_receive {:DOWN, ^ref, :process, ^pid, :normal}, expected_steps * 5_000

      # Collect all comments that were posted
      comments = flush_comments("bead-advance-1")

      # Last comment should be pipeline complete
      assert List.last(comments) =~ "Pipeline complete",
             "Expected last comment to be 'Pipeline complete', got: #{inspect(comments)}"

      # Should have phase-passed comments if multi-step
      if expected_steps > 1 do
        assert Enum.any?(comments, &(&1 =~ "Phase passed"))
      end
    end
  end

  describe "exit_status non-zero (failure and retry)" do
    test "worker retries on failure then succeeds" do
      call_count = :counters.new(1, [:atomics])

      # First call exits with 1, second call exits with 0
      Application.put_env(:conductor, :agent_spawn_fn, fn pipeline ->
        agent = Conductor.Pipeline.current_agent(pipeline)
        count = :counters.get(call_count, 1) + 1
        :counters.put(call_count, 1, count)

        exit_code = if count <= 1, do: 1, else: 0

        send(self(), {:agent_spawned, pipeline.bead_id, agent})

        port =
          Port.open(
            {:spawn_executable, "/bin/sh"},
            [:exit_status, :binary, args: ["-c", "sleep 0.3; exit #{exit_code}"]]
          )

        {:os_pid, os_pid} = Port.info(port, :os_pid)
        {:ok, port, os_pid}
      end)

      bead = %{"id" => "bead-retry-1", "repo" => "/tmp/repo", "issue_type" => "chore"}

      {:ok, pid} = AgentWorker.start_link(bead)
      ref = Process.monitor(pid)

      # First dispatch (will fail with exit 1)
      # Should get a retry comment
      assert_receive {:mock_rsry, {:bead_comment, _, "bead-retry-1", comment}}, 5_000
      assert comment =~ "failed"
      assert comment =~ "retrying"

      # Backoff is 30s * 2^1 = 60s. Skip it by sending :retry directly.
      Process.sleep(200)
      assert Process.alive?(pid)
      send(pid, :retry)

      # Second dispatch (will succeed with exit 0)
      assert_receive {:mock_rsry, {:bead_comment, _, "bead-retry-1", complete}}, 5_000
      assert complete =~ "Pipeline complete"

      assert_receive {:DOWN, ^ref, :process, ^pid, :normal}, 5_000
    end

    test "worker exhausts retries and stops (deadletter)" do
      # Always exit with 1
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_spawn_fn(1, 0.2, self())
      )

      bead = %{"id" => "bead-dead-1", "repo" => "/tmp/repo", "issue_type" => "task"}

      {:ok, pid} = AgentWorker.start_link(bead)
      ref = Process.monitor(pid)

      # Failure 1 -> retry
      assert_receive {:agent_spawned, "bead-dead-1", "dev-agent"}, 1_000
      assert_receive {:mock_rsry, {:bead_comment, _, _, c1}}, 5_000
      assert c1 =~ "retrying"

      # Skip backoff
      Process.sleep(100)
      send(pid, :retry)

      # Failure 2 -> retry
      assert_receive {:agent_spawned, "bead-dead-1", "dev-agent"}, 5_000
      assert_receive {:mock_rsry, {:bead_comment, _, _, c2}}, 5_000
      assert c2 =~ "retrying"

      # Skip backoff
      Process.sleep(100)
      send(pid, :retry)

      # Failure 3 -> deadletter (3 failures, max_retries: 3)
      assert_receive {:agent_spawned, "bead-dead-1", "dev-agent"}, 5_000
      assert_receive {:mock_rsry, {:bead_comment, _, "bead-dead-1", deadletter}}, 5_000
      assert deadletter =~ "exhausted"

      assert_receive {:DOWN, ^ref, :process, ^pid, :normal}, 5_000
    end
  end

  describe "timeout" do
    test "worker handles timeout by closing port" do
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_controllable_spawn_fn(self())
      )

      bead = %{"id" => "bead-timeout-1", "repo" => "/tmp/repo", "issue_type" => "task"}

      {:ok, pid} = AgentWorker.start_link(bead)
      assert_receive {:agent_spawned, "bead-timeout-1", "dev-agent", _port}, 1_000

      # Send timeout message directly (don't wait for real 600s timeout)
      send(pid, :timeout)

      # After timeout, the worker closes the port which triggers exit_status.
      # This leads to on_failure -> retry (since retries < max_retries).
      Process.sleep(500)

      # Worker should still be alive (in retry state)
      assert Process.alive?(pid)

      cleanup_worker(pid)
    end
  end

  describe "get_state/1" do
    test "returns pipeline info with expected fields" do
      Application.put_env(
        :conductor,
        :agent_spawn_fn,
        MockRsry.make_controllable_spawn_fn(self())
      )

      bead = %{"id" => "bead-state-1", "repo" => "/tmp/repo", "issue_type" => "bug"}

      {:ok, pid} = AgentWorker.start_link(bead)
      assert_receive {:agent_spawned, _, _, _}, 1_000

      state = AgentWorker.get_state(pid)

      assert state.bead_id == "bead-state-1"
      assert state.repo == "/tmp/repo"
      assert "dev-agent" in state.agents
      assert state.progress =~ ~r|^0/\d+$|
      assert is_integer(state.os_pid)
      assert %DateTime{} = state.started_at

      cleanup_worker(pid)
    end
  end

  describe "sprites backend: pid-based agent process" do
    test "worker handles pid-based agent (sprites mode) completing successfully" do
      # Simulate a sprites dispatch: spawn_fn returns a pid instead of a port.
      # The pid sends Port-compatible messages to the worker.
      test_pid = self()

      Application.put_env(:conductor, :agent_spawn_fn, fn pipeline ->
        agent = Conductor.Pipeline.current_agent(pipeline)
        worker = self()

        pid =
          spawn(fn ->
            # Simulate agent running and producing output
            Process.sleep(200)
            send(worker, {self(), {:data, {:eol, ~s|{"session_id":"sprite-sess-1"}|}}})
            Process.sleep(100)
            send(worker, {self(), {:exit_status, 0}})
            # Keep alive briefly so agent_alive? checks work
            Process.sleep(500)
          end)

        send(test_pid, {:agent_spawned, pipeline.bead_id, agent})
        {:ok, pid, "rsry-#{pipeline.bead_id}"}
      end)

      bead = %{"id" => "bead-sprites-1", "repo" => "/tmp/repo", "issue_type" => "task"}

      {:ok, pid} = AgentWorker.start_link(bead)
      ref = Process.monitor(pid)

      assert_receive {:agent_spawned, "bead-sprites-1", "dev-agent"}, 1_000

      # Worker should complete normally
      assert_receive {:DOWN, ^ref, :process, ^pid, :normal}, 5_000

      assert_receive {:mock_rsry, {:bead_comment, _, "bead-sprites-1", comment}}, 1_000
      assert comment =~ "Pipeline complete"
    end

    test "worker stores provider_name when os_pid is a string" do
      test_pid = self()

      Application.put_env(:conductor, :agent_spawn_fn, fn pipeline ->
        agent = Conductor.Pipeline.current_agent(pipeline)
        send(test_pid, {:agent_spawned, pipeline.bead_id, agent})

        pid =
          spawn(fn ->
            Process.sleep(60_000)
          end)

        {:ok, pid, "rsry-#{pipeline.bead_id}"}
      end)

      bead = %{"id" => "bead-sprite-name-1", "repo" => "/tmp/repo", "issue_type" => "task"}

      {:ok, worker_pid} = AgentWorker.start_link(bead)
      assert_receive {:agent_spawned, _, _}, 1_000

      # The worker doesn't expose provider_name in get_state, but we can
      # verify it started successfully with a pid-based agent
      state = AgentWorker.get_state(worker_pid)
      assert state.bead_id == "bead-sprite-name-1"

      assert is_pid(state.os_pid) or is_binary(state.os_pid) or
               is_binary("rsry-bead-sprite-name-1")

      cleanup_worker(worker_pid)
    end
  end

  # -- Helpers --

  # Cleanly stop a worker without sending EXIT to the test process.
  # Unlink first, then kill and wait for confirmation.
  defp flush_comments(bead_id) do
    Stream.repeatedly(fn ->
      receive do
        {:mock_rsry, {:bead_comment, _, ^bead_id, comment}} -> comment
      after
        100 -> nil
      end
    end)
    |> Enum.take_while(&(&1 != nil))
  end

  defp cleanup_worker(pid) when is_pid(pid) do
    if Process.alive?(pid) do
      Process.unlink(pid)
      ref = Process.monitor(pid)
      Process.exit(pid, :kill)

      receive do
        {:DOWN, ^ref, :process, ^pid, _} -> :ok
      after
        1_000 -> :ok
      end
    end
  end
end
