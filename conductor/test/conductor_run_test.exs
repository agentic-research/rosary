defmodule Mix.Tasks.Conductor.RunTest do
  use ExUnit.Case, async: true

  alias Mix.Tasks.Conductor.Run

  describe "parse_args/1" do
    test "returns defaults when no args" do
      assert Run.parse_args([]) == %{log: nil, interval: nil, max: nil}
    end

    test "parses --log" do
      assert %{log: "/tmp/conductor.log"} = Run.parse_args(["--log", "/tmp/conductor.log"])
    end

    test "parses --interval" do
      assert %{interval: 60_000} = Run.parse_args(["--interval", "60000"])
    end

    test "parses --max" do
      assert %{max: 5} = Run.parse_args(["--max", "5"])
    end

    test "parses all options together" do
      args = ["--log", "/var/log/conductor.log", "--interval", "15000", "--max", "2"]

      assert Run.parse_args(args) == %{
               log: "/var/log/conductor.log",
               interval: 15_000,
               max: 2
             }
    end

    test "ignores unknown flags" do
      assert Run.parse_args(["--verbose", "--log", "out.log"]) == %{
               log: "out.log",
               interval: nil,
               max: nil
             }
    end

    test "handles interleaved positional args" do
      assert %{log: "daemon.log", max: 4} =
               Run.parse_args(["some-arg", "--log", "daemon.log", "--max", "4"])
    end
  end
end
