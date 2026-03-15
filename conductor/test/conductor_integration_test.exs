defmodule Conductor.IntegrationTest do
  @moduledoc """
  Integration tests that exercise the full supervision tree.

  Tagged with :integration -- excluded by default in test_helper.exs.
  Run with: mix test --include integration
  """
  use ExUnit.Case, async: false

  @moduletag :integration

  alias Conductor.{AgentSupervisor, Pipeline, RsryClient}

  describe "supervision tree" do
    test "full app starts with supervision tree" do
      # The application is started automatically by mix test.
      # Verify all named processes are running.
      assert GenServer.whereis(Conductor.RsryClient) != nil,
             "RsryClient should be running"

      assert GenServer.whereis(Conductor.AgentSupervisor) != nil,
             "AgentSupervisor should be running"

      assert GenServer.whereis(Conductor.Orchestrator) != nil,
             "Orchestrator should be running"
    end

    test "supervisor is accessible and reports zero active agents" do
      assert AgentSupervisor.active_count() == 0
      assert AgentSupervisor.which_agents() == []
    end

    test "RsryClient responds to status (requires rsry running)" do
      case RsryClient.status() do
        {:ok, result} ->
          assert is_map(result)

        {:error, _reason} ->
          # rsry not running -- client should still be alive
          assert Process.alive?(GenServer.whereis(Conductor.RsryClient))
      end
    end

    test "RsryClient connected? returns boolean" do
      result = RsryClient.connected?()
      assert is_boolean(result)
    end
  end

  describe "pipeline to agent flow" do
    test "Pipeline.for_bead builds valid pipeline for dispatch" do
      pipeline = Pipeline.for_bead("integration-1", "/tmp/test-repo", "bug")

      assert pipeline.bead_id == "integration-1"
      assert Pipeline.current_agent(pipeline) == "dev-agent"
      assert Pipeline.step_count(pipeline) == 2
      assert Pipeline.agents(pipeline) == ["dev-agent", "staging-agent"]

      # Roundtrip serialization
      map = Pipeline.to_map(pipeline)
      restored = Pipeline.from_map(map)
      assert restored.bead_id == pipeline.bead_id
      assert Pipeline.agents(restored) == Pipeline.agents(pipeline)
    end

    test "AgentSupervisor.start_agent dispatches a worker (requires rsry)" do
      bead = %{
        "id" => "integration-test-#{System.unique_integer([:positive])}",
        "repo" => "/tmp/integration-test",
        "issue_type" => "task"
      }

      case AgentSupervisor.start_agent(bead) do
        {:ok, pid} ->
          assert Process.alive?(pid)
          ref = Process.monitor(pid)
          state = Conductor.AgentWorker.get_state(pid)
          assert state.bead_id == bead["id"]
          Process.exit(pid, :kill)
          assert_receive {:DOWN, ^ref, :process, ^pid, :killed}, 5_000

        {:error, {:start_failed, _reason}} ->
          # claude not on PATH or repo doesn't exist -- expected in CI
          :ok

        {:error, :max_children} ->
          # test config sets max_concurrent: 0
          :ok
      end
    end

    test "RsryClient.list_beads returns structured data (requires rsry)" do
      case RsryClient.list_beads("open") do
        {:ok, %{"beads" => beads}} ->
          assert is_list(beads)

          for bead <- beads do
            assert Map.has_key?(bead, "id")
          end

        {:error, _} ->
          :ok
      end
    end

    test "RsryClient.scan triggers repo scan (requires rsry)" do
      case RsryClient.scan() do
        {:ok, _result} -> :ok
        {:error, _} -> :ok
      end

      assert Process.alive?(GenServer.whereis(Conductor.RsryClient))
    end
  end

  describe "full pipeline with spawn_fn" do
    test "Pipeline.for_bead -> AgentSupervisor.start_agent -> worker runs to completion" do
      test_pid = self()

      # Use spawn_fn to avoid needing claude binary
      Application.put_env(:conductor, :agent_spawn_fn, fn pipeline ->
        agent = Pipeline.current_agent(pipeline)
        send(test_pid, {:integration_spawn, pipeline.bead_id, agent})

        # Exit successfully after 0.3s
        port =
          Port.open(
            {:spawn_executable, "/bin/sh"},
            [:exit_status, :binary, args: ["-c", "sleep 0.3; exit 0"]]
          )

        {:os_pid, os_pid} = Port.info(port, :os_pid)
        {:ok, port, os_pid}
      end)

      # Temporarily raise max_children to allow dispatch
      Application.put_env(:conductor, :max_concurrent, 5)

      # Restart supervisor with new max_children
      case GenServer.whereis(Conductor.AgentSupervisor) do
        nil -> :ok
        pid -> GenServer.stop(pid, :normal, 5_000)
      end

      Process.sleep(10)

      case Conductor.AgentSupervisor.start_link() do
        {:ok, _} -> :ok
        {:error, {:already_started, _}} -> :ok
      end

      bead = %{
        "id" => "integ-full-#{System.unique_integer([:positive])}",
        "repo" => "/tmp/integ",
        "issue_type" => "task"
      }

      case AgentSupervisor.start_agent(bead) do
        {:ok, pid} ->
          ref = Process.monitor(pid)

          assert_receive {:integration_spawn, _, "dev-agent"}, 2_000
          assert_receive {:DOWN, ^ref, :process, ^pid, :normal}, 5_000

        {:error, reason} ->
          flunk("Expected agent to start, got error: #{inspect(reason)}")
      end

      # Cleanup
      Application.delete_env(:conductor, :agent_spawn_fn)
      Application.put_env(:conductor, :max_concurrent, 0)
    end
  end
end
