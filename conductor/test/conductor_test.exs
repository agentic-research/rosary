defmodule ConductorTest do
  use ExUnit.Case

  test "status returns a map" do
    status = Conductor.status()
    assert is_map(status)
    assert Map.has_key?(status, :connected)
    assert Map.has_key?(status, :active_agents)
    assert Map.has_key?(status, :orchestrator)
  end
end
