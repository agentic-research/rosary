defmodule ConductorTest do
  use ExUnit.Case
  doctest Conductor

  test "greets the world" do
    assert Conductor.hello() == :world
  end
end
