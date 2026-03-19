defmodule Conductor.Test.MockSprites do
  @moduledoc """
  Mock replacement for Conductor.SpritesClient.

  Records all API calls and sends them to the test process.
  Configurable responses for each operation.

  Usage in tests:

      Application.put_env(:conductor, :sprites_client_mod, Conductor.Test.MockSprites)
      {:ok, _} = MockSprites.start_link(test_pid: self())
      # code that calls SpritesClient functions...
      assert_receive {:mock_sprites, {:create_sprite, "rsry-bead-1", %{}}}
  """
  use GenServer

  @behaviour Conductor.SpritesClient

  defstruct [
    :test_pid,
    create_fn: nil,
    destroy_fn: nil,
    exec_fn: nil,
    policy_fn: nil,
    calls: []
  ]

  # -- Public API --

  def start_link(opts \\ []) do
    GenServer.start_link(__MODULE__, opts, name: __MODULE__)
  end

  def get_calls(pid \\ __MODULE__) do
    GenServer.call(pid, :get_calls)
  end

  def clear_calls(pid \\ __MODULE__) do
    GenServer.call(pid, :clear_calls)
  end

  def set_create_response(pid \\ __MODULE__, fun) do
    GenServer.call(pid, {:set_fn, :create_fn, fun})
  end

  def set_destroy_response(pid \\ __MODULE__, fun) do
    GenServer.call(pid, {:set_fn, :destroy_fn, fun})
  end

  def set_exec_response(pid \\ __MODULE__, fun) do
    GenServer.call(pid, {:set_fn, :exec_fn, fun})
  end

  # -- SpritesClient callbacks (delegated to GenServer) --

  @impl Conductor.SpritesClient
  def create_sprite(name, opts \\ %{}) do
    GenServer.call(__MODULE__, {:create_sprite, name, opts})
  end

  @impl Conductor.SpritesClient
  def destroy_sprite(name) do
    GenServer.call(__MODULE__, {:destroy_sprite, name})
  end

  @impl Conductor.SpritesClient
  def exec_sync(name, command, env \\ %{}) do
    GenServer.call(__MODULE__, {:exec_sync, name, command, env})
  end

  @impl Conductor.SpritesClient
  def set_network_policy(name, policy \\ %{}) do
    GenServer.call(__MODULE__, {:set_network_policy, name, policy})
  end

  @impl Conductor.SpritesClient
  def exec_ws_url(name) do
    "ws://localhost:9999/v1/sprites/#{name}/exec"
  end

  # -- GenServer callbacks --

  @impl true
  def init(opts) do
    {:ok,
     %__MODULE__{
       test_pid: opts[:test_pid],
       create_fn: opts[:create_fn],
       destroy_fn: opts[:destroy_fn],
       exec_fn: opts[:exec_fn],
       policy_fn: opts[:policy_fn]
     }}
  end

  @impl true
  def handle_call({:create_sprite, name, opts}, _from, state) do
    call = {:create_sprite, name, opts}
    if state.test_pid, do: send(state.test_pid, {:mock_sprites, call})

    result =
      if state.create_fn do
        state.create_fn.(name, opts)
      else
        {:ok, %{"name" => name, "status" => "running"}}
      end

    {:reply, result, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:destroy_sprite, name}, _from, state) do
    call = {:destroy_sprite, name}
    if state.test_pid, do: send(state.test_pid, {:mock_sprites, call})

    result =
      if state.destroy_fn do
        state.destroy_fn.(name)
      else
        :ok
      end

    {:reply, result, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:exec_sync, name, command, env}, _from, state) do
    call = {:exec_sync, name, command, env}
    if state.test_pid, do: send(state.test_pid, {:mock_sprites, call})

    result =
      if state.exec_fn do
        state.exec_fn.(name, command, env)
      else
        {:ok, %{"exit_code" => 0, "stdout" => "", "stderr" => ""}}
      end

    {:reply, result, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call({:set_network_policy, name, policy}, _from, state) do
    call = {:set_network_policy, name, policy}
    if state.test_pid, do: send(state.test_pid, {:mock_sprites, call})

    result =
      if state.policy_fn do
        state.policy_fn.(name, policy)
      else
        :ok
      end

    {:reply, result, %{state | calls: state.calls ++ [call]}}
  end

  def handle_call(:get_calls, _from, state) do
    {:reply, state.calls, state}
  end

  def handle_call(:clear_calls, _from, state) do
    {:reply, :ok, %{state | calls: []}}
  end

  def handle_call({:set_fn, key, fun}, _from, state) do
    {:reply, :ok, Map.put(state, key, fun)}
  end
end
