defmodule Conductor.Test.MockRsry do
  @moduledoc """
  Mock replacement for Conductor.RsryClient GenServer.

  Registers under the same name (Conductor.RsryClient) so that production
  code calling RsryClient.list_beads/1, RsryClient.bead_comment/3, etc.
  hits this process instead.

  Usage in tests:

      {:ok, mock} = MockRsry.start_link(test_pid: self())
      # code that calls RsryClient functions...
      assert_receive {:mock_rsry, {:bead_comment, _repo, _id, _body}}
  """
  use GenServer

  defstruct [
    :test_pid,
    list_beads_fn: nil,
    comment_fn: nil,
    calls: []
  ]

  # -- Public API --

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: Conductor.RsryClient)
  end

  @doc "Set a custom list_beads response function."
  def set_list_beads_response(pid \\ Conductor.RsryClient, fun) do
    GenServer.call(pid, {:set_list_beads_fn, fun})
  end

  @doc "Set a custom bead_comment response function."
  def set_comment_response(pid \\ Conductor.RsryClient, fun) do
    GenServer.call(pid, {:set_comment_fn, fun})
  end

  @doc "Get all recorded calls."
  def get_calls(pid \\ Conductor.RsryClient) do
    GenServer.call(pid, :get_calls)
  end

  @doc "Clear recorded calls."
  def clear_calls(pid \\ Conductor.RsryClient) do
    GenServer.call(pid, :clear_calls)
  end

  # -- GenServer callbacks --

  @impl true
  def init(opts) do
    {:ok,
     %__MODULE__{
       test_pid: opts[:test_pid],
       list_beads_fn: opts[:list_beads_fn],
       comment_fn: opts[:comment_fn]
     }}
  end

  @impl true
  def handle_call({:tool, "rsry_list_beads", args}, _from, state) do
    status = args[:status] || args["status"]
    call = {:list_beads, status}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})

    result =
      if state.list_beads_fn do
        state.list_beads_fn.(status)
      else
        {:ok, %{"beads" => []}}
      end

    {:reply, result, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_bead_comment", args}, _from, state) do
    repo = args[:repo_path] || args["repo_path"]
    id = args[:id] || args["id"]
    body = args[:body] || args["body"]

    call = {:bead_comment, repo, id, body}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})

    result =
      if state.comment_fn do
        state.comment_fn.(repo, id, body)
      else
        {:ok, %{"ok" => true}}
      end

    {:reply, result, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_bead_close", args}, _from, state) do
    repo = args[:repo_path] || args["repo_path"]
    id = args[:id] || args["id"]
    call = {:bead_close, repo, id}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})
    {:reply, {:ok, %{"ok" => true}}, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_scan", _args}, _from, state) do
    call = {:scan}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})
    {:reply, {:ok, %{"ok" => true}}, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_status", _args}, _from, state) do
    call = {:status}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})
    {:reply, {:ok, %{"ok" => true}}, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_active", _args}, _from, state) do
    call = {:active}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})
    {:reply, {:ok, %{"ok" => true}}, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_dispatch", args}, _from, state) do
    call = {:dispatch, args}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})
    {:reply, {:ok, %{"ok" => true}}, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_bead_search", args}, _from, state) do
    call = {:bead_search, args}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})
    {:reply, {:ok, %{"beads" => []}}, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_workspace_create", args}, _from, state) do
    bead_id = args[:bead_id] || args["bead_id"]
    repo_path = args[:repo_path] || args["repo_path"]
    call = {:workspace_create, bead_id, repo_path}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})

    {:reply,
     {:ok,
      %{"bead_id" => bead_id, "work_dir" => repo_path, "vcs" => "None", "repo_path" => repo_path}},
     %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_workspace_checkpoint", args}, _from, state) do
    bead_id = args[:bead_id] || args["bead_id"]
    repo_path = args[:repo_path] || args["repo_path"]
    call = {:workspace_checkpoint, bead_id, repo_path}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})

    {:reply, {:ok, %{"bead_id" => bead_id, "change_id" => nil, "vcs" => "None"}},
     %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, "rsry_workspace_cleanup", args}, _from, state) do
    bead_id = args[:bead_id] || args["bead_id"]
    repo_path = args[:repo_path] || args["repo_path"]
    call = {:workspace_cleanup, bead_id, repo_path}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})

    {:reply, {:ok, %{"bead_id" => bead_id, "cleaned" => true}},
     %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:tool, name, _args}, _from, state) do
    call = {:unknown_tool, name}
    if state.test_pid, do: send(state.test_pid, {:mock_rsry, call})

    {:reply, {:error, %{"message" => "unknown tool: #{name}"}},
     %{state | calls: state.calls ++ [call]}}
  end

  def handle_call(:connected?, _from, state) do
    {:reply, true, state}
  end

  def handle_call({:set_list_beads_fn, fun}, _from, state) do
    {:reply, :ok, %{state | list_beads_fn: fun}}
  end

  def handle_call({:set_comment_fn, fun}, _from, state) do
    {:reply, :ok, %{state | comment_fn: fun}}
  end

  def handle_call(:get_calls, _from, state) do
    {:reply, state.calls, state}
  end

  def handle_call(:clear_calls, _from, state) do
    {:reply, :ok, %{state | calls: []}}
  end

  @impl true
  def handle_info(_msg, state), do: {:noreply, state}

  # -- Test Spawn Helpers --

  @doc """
  Create a spawn function for use with :agent_spawn_fn that opens a Port
  which exits with the given code after a delay.

  The function notifies `test_pid` when spawn happens.
  Returns {:ok, port, os_pid}.
  """
  def make_spawn_fn(exit_code, delay_s \\ 1, test_pid \\ nil) do
    fn pipeline ->
      agent = Conductor.Pipeline.current_agent(pipeline)
      if test_pid, do: send(test_pid, {:agent_spawned, pipeline.bead_id, agent})

      port =
        Port.open(
          {:spawn_executable, "/bin/sh"},
          [
            :exit_status,
            :binary,
            args: ["-c", "sleep #{delay_s}; exit #{exit_code}"]
          ]
        )

      {:os_pid, os_pid} = Port.info(port, :os_pid)
      {:ok, port, os_pid}
    end
  end

  @doc """
  Create a spawn function controlled by the test process.

  The test process receives {:agent_spawned, bead_id, agent, port} and
  can later inject exit_status messages by sending {port, {:exit_status, code}}
  to the worker.

  The spawned process runs for `delay_s` seconds (long enough to inject messages).
  """
  def make_controllable_spawn_fn(test_pid, delay_s \\ 60) do
    fn pipeline ->
      agent = Conductor.Pipeline.current_agent(pipeline)

      port =
        Port.open(
          {:spawn_executable, "/bin/sh"},
          [
            :exit_status,
            :binary,
            args: ["-c", "sleep #{delay_s}"]
          ]
        )

      {:os_pid, os_pid} = Port.info(port, :os_pid)
      send(test_pid, {:agent_spawned, pipeline.bead_id, agent, port})
      {:ok, port, os_pid}
    end
  end

  @doc """
  Create a spawn function that always fails.
  """
  def make_failing_spawn_fn(reason \\ "connection refused") do
    fn _pipeline ->
      {:error, reason}
    end
  end
end
