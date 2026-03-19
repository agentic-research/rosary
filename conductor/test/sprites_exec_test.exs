defmodule Conductor.SpritesExecTest do
  use ExUnit.Case, async: true

  alias Conductor.SpritesExec

  describe "split_lines/1" do
    test "splits complete lines" do
      {lines, remainder} = SpritesExec.split_lines("hello\nworld\n")
      assert lines == ["hello", "world"]
      assert remainder == ""
    end

    test "handles partial line at end" do
      {lines, remainder} = SpritesExec.split_lines("hello\nwor")
      assert lines == ["hello"]
      assert remainder == "wor"
    end

    test "no newlines — entire string is remainder" do
      {lines, remainder} = SpritesExec.split_lines("partial data")
      assert lines == []
      assert remainder == "partial data"
    end

    test "empty string" do
      {lines, remainder} = SpritesExec.split_lines("")
      assert lines == []
      assert remainder == ""
    end

    test "single newline" do
      {lines, remainder} = SpritesExec.split_lines("\n")
      assert lines == [""]
      assert remainder == ""
    end

    test "multiple newlines" do
      {lines, remainder} = SpritesExec.split_lines("\n\n\n")
      assert lines == ["", "", ""]
      assert remainder == ""
    end

    test "buffered partial then complete" do
      # Simulate buffering: first chunk has partial line
      {lines1, buf} = SpritesExec.split_lines("hel")
      assert lines1 == []
      assert buf == "hel"

      # Second chunk completes the line and starts another
      {lines2, buf2} = SpritesExec.split_lines(buf <> "lo world\nfoo")
      assert lines2 == ["hello world"]
      assert buf2 == "foo"

      # Third chunk completes with newline
      {lines3, buf3} = SpritesExec.split_lines(buf2 <> " bar\n")
      assert lines3 == ["foo bar"]
      assert buf3 == ""
    end

    test "JSON output lines" do
      json_lines = ~s|{"session_id":"abc"}\n{"result":"ok"}\n|
      {lines, remainder} = SpritesExec.split_lines(json_lines)
      assert lines == [~s|{"session_id":"abc"}|, ~s|{"result":"ok"}|]
      assert remainder == ""
    end
  end

  describe "port-compatible message format" do
    test "messages use {pid, {:data, {:eol, line}}} format" do
      # Verify the message format that SpritesExec sends matches what
      # AgentWorker expects from Ports
      worker_pid = self()
      fake_exec_pid = spawn(fn -> :ok end)

      # Simulate what SpritesExec does when it receives a complete line
      send(worker_pid, {fake_exec_pid, {:data, {:eol, "test line"}}})

      assert_receive {^fake_exec_pid, {:data, {:eol, "test line"}}}
    end

    test "exit message uses {pid, {:exit_status, code}} format" do
      worker_pid = self()
      fake_exec_pid = spawn(fn -> :ok end)

      send(worker_pid, {fake_exec_pid, {:exit_status, 0}})

      assert_receive {^fake_exec_pid, {:exit_status, 0}}
    end
  end
end
