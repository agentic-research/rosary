defmodule Conductor.SpritesClientTest do
  use ExUnit.Case, async: true

  alias Conductor.SpritesClient

  describe "sprite_name/1" do
    test "derives deterministic name from bead ID" do
      assert SpritesClient.sprite_name("bead-abc123") == "rsry-bead-abc123"
      assert SpritesClient.sprite_name("xyz") == "rsry-xyz"
    end
  end

  describe "default_network_policy/0" do
    test "returns deny-by-default policy with expected domains" do
      policy = SpritesClient.default_network_policy()

      assert policy["default"] == "deny"
      assert is_list(policy["allowed_domains"])
      assert "api.anthropic.com" in policy["allowed_domains"]
      assert "github.com" in policy["allowed_domains"]
      assert "crates.io" in policy["allowed_domains"]
    end
  end

  describe "exec_ws_url/1" do
    test "generates WSS URL from sprite name" do
      # Override base URL for test
      Application.put_env(:conductor, :sprites_base_url, "https://api.sprites.dev/v1")

      url = SpritesClient.exec_ws_url("rsry-bead-1")

      assert url == "wss://api.sprites.dev/v1/sprites/rsry-bead-1/exec"
    end

    test "handles http base URL" do
      Application.put_env(:conductor, :sprites_base_url, "http://localhost:8080/v1")

      url = SpritesClient.exec_ws_url("test-sprite")

      assert url == "ws://localhost:8080/v1/sprites/test-sprite/exec"
    end
  end
end
