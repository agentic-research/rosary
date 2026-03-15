defmodule Conductor.RsryClientTest do
  use ExUnit.Case

  # Integration tests — require rsry serve running on :8383

  @tag :integration
  test "connected? returns true when rsry is running" do
    assert Conductor.RsryClient.connected?()
  end

  @tag :integration
  test "status returns aggregated counts" do
    {:ok, result} = Conductor.RsryClient.status()
    assert is_map(result)
  end

  @tag :integration
  test "list_beads returns beads" do
    {:ok, result} = Conductor.RsryClient.list_beads()
    assert is_map(result)
  end

  @tag :integration
  test "active returns agents list" do
    {:ok, result} = Conductor.RsryClient.active()
    assert is_map(result)
    assert Map.has_key?(result, "active")
  end

  @tag :integration
  test "bead_search returns results" do
    {:ok, result} = Conductor.RsryClient.bead_search("/Users/jamesgardner/remotes/art/rosary", "conductor")
    assert is_map(result)
  end
end
