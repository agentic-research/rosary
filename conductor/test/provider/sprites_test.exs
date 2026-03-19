defmodule Conductor.Provider.SpritesTest do
  use ExUnit.Case, async: false

  alias Conductor.Provider.Sprites
  alias Conductor.Test.MockSprites

  setup do
    Application.put_env(:conductor, :sprites_client_mod, MockSprites)
    Application.put_env(:conductor, :github_token, "test-gh-token")
    Application.put_env(:conductor, :anthropic_api_key, "test-api-key")

    {:ok, _} = MockSprites.start_link(test_pid: self())

    on_exit(fn ->
      Application.delete_env(:conductor, :sprites_client_mod)
      Application.delete_env(:conductor, :github_token)
      Application.delete_env(:conductor, :anthropic_api_key)

      if Process.whereis(MockSprites), do: GenServer.stop(MockSprites)
    end)

    :ok
  end

  describe "provision/3" do
    test "creates sprite, sets network policy, and clones repo" do
      assert :ok == Sprites.provision("rsry-bead-1", "agentic-research/rosary", %{})

      assert_receive {:mock_sprites, {:create_sprite, "rsry-bead-1", %{}}}
      assert_receive {:mock_sprites, {:set_network_policy, "rsry-bead-1", _}}
      assert_receive {:mock_sprites, {:exec_sync, "rsry-bead-1", clone_cmd, %{}}}
      assert clone_cmd =~ "git clone"
      assert clone_cmd =~ "test-gh-token"
      assert clone_cmd =~ "agentic-research/rosary"
    end

    test "returns error if create_sprite fails" do
      MockSprites.set_create_response(fn _name, _opts ->
        {:error, {:http, 500, "internal error"}}
      end)

      assert {:error, _} = Sprites.provision("rsry-fail", "org/repo", %{})
    end
  end

  describe "deprovision/1" do
    test "destroys the sprite" do
      assert :ok == Sprites.deprovision("rsry-bead-1")
      assert_receive {:mock_sprites, {:destroy_sprite, "rsry-bead-1"}}
    end
  end

  describe "exec_sync/3" do
    test "runs command via sprites client and returns output" do
      MockSprites.set_exec_response(fn _name, _cmd, _env ->
        {:ok, %{"exit_code" => 0, "stdout" => "ok\n"}}
      end)

      assert {:ok, {"ok\n", 0}} == Sprites.exec_sync("rsry-bead-1", "task test", "/workspace")
    end

    test "handles non-zero exit codes" do
      MockSprites.set_exec_response(fn _name, _cmd, _env ->
        {:ok, %{"exit_code" => 1, "stdout" => "FAIL"}}
      end)

      assert {:ok, {"FAIL", 1}} == Sprites.exec_sync("rsry-bead-1", "task test", "/workspace")
    end
  end

  describe "alive?/1" do
    test "returns true for alive pid" do
      pid = spawn(fn -> Process.sleep(60_000) end)
      assert Sprites.alive?(pid)
      Process.exit(pid, :kill)
    end

    test "returns false for dead pid" do
      pid = spawn(fn -> :ok end)
      Process.sleep(50)
      refute Sprites.alive?(pid)
    end

    test "returns false for nil" do
      refute Sprites.alive?(nil)
    end
  end

  describe "stop_process/1" do
    test "stops a living process" do
      pid = spawn(fn -> Process.sleep(60_000) end)
      assert :ok == Sprites.stop_process(pid)
    end
  end
end
