defmodule Conductor.Provider.LocalTest do
  use ExUnit.Case, async: true

  alias Conductor.Provider.Local

  describe "provision/3" do
    test "is a no-op" do
      assert :ok == Local.provision("test", "/tmp/repo", %{})
    end
  end

  describe "deprovision/1" do
    test "is a no-op" do
      assert :ok == Local.deprovision("test")
    end
  end

  describe "exec_sync/3" do
    test "runs command and returns output with exit code" do
      assert {:ok, {"hello\n", 0}} == Local.exec_sync("test", "echo hello", "/tmp")
    end

    test "returns non-zero exit code on failure" do
      {:ok, {_output, code}} = Local.exec_sync("test", "exit 42", "/tmp")
      assert code == 42
    end
  end

  describe "alive?/1" do
    test "returns false for non-port values" do
      refute Local.alive?(nil)
      refute Local.alive?(self())
    end
  end
end
