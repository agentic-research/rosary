defmodule Conductor.Provider.LocalTest do
  use ExUnit.Case, async: false

  alias Conductor.Provider.Local

  # Each test that changes application env must restore it
  setup do
    original_mode = Application.get_env(:conductor, :dispatch_mode)

    on_exit(fn ->
      if original_mode do
        Application.put_env(:conductor, :dispatch_mode, original_mode)
      else
        Application.delete_env(:conductor, :dispatch_mode)
      end
    end)

    :ok
  end

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

  describe "spawn_process/6 CLI mode — exit_status" do
    test "receives exit_status 0 for successful command" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, os_pid} = Local.spawn_process("t", sh, ["-c", "exit 0"], "/tmp", %{}, self())
      assert is_port(port)
      assert is_integer(os_pid)

      assert_receive {^port, {:exit_status, 0}}, 5_000
    end

    test "receives exit_status for non-zero exit code" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, _pid} = Local.spawn_process("t", sh, ["-c", "exit 42"], "/tmp", %{}, self())

      assert_receive {^port, {:exit_status, 42}}, 5_000
    end

    test "receives exit_status when command crashes with signal" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, _pid} =
        Local.spawn_process("t", sh, ["-c", "kill -9 $$"], "/tmp", %{}, self())

      # SIGKILL = signal 9, exit status = 128 + 9 = 137
      assert_receive {^port, {:exit_status, status}}, 5_000
      # On some systems it might be 137 (128+9) or 9, depending on shell
      assert status != 0
    end
  end

  describe "spawn_process/6 CLI mode — stdout capture" do
    test "captures stdout lines as {:data, {:eol, line}} messages" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, _pid} =
        Local.spawn_process(
          "t",
          sh,
          ["-c", "echo hello; echo world"],
          "/tmp",
          %{},
          self()
        )

      # Collect data messages
      lines = collect_lines(port, 2, 5_000)
      assert "hello" in lines
      assert "world" in lines

      # Also get exit status
      assert_receive {^port, {:exit_status, 0}}, 5_000
    end

    test "handles long lines without truncation" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      # Generate a line longer than a typical buffer but within our 65536 limit
      long_line = String.duplicate("x", 1000)

      {:ok, port, _pid} =
        Local.spawn_process(
          "t",
          sh,
          ["-c", "echo '#{long_line}'"],
          "/tmp",
          %{},
          self()
        )

      lines = collect_lines(port, 1, 5_000)
      assert long_line in lines

      assert_receive {^port, {:exit_status, 0}}, 5_000
    end
  end

  describe "spawn_process/6 CLI mode — stdin is /dev/null" do
    test "agent reading stdin gets EOF immediately (not blocked)" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      # If stdin is /dev/null, `read` returns immediately with exit 1 (EOF)
      # If stdin were a pipe, `read` would block forever
      {:ok, port, _pid} =
        Local.spawn_process(
          "t",
          sh,
          ["-c", "read LINE; echo \"got: $LINE\"; exit 0"],
          "/tmp",
          %{},
          self()
        )

      # Should complete quickly (not hang waiting for stdin)
      assert_receive {^port, {:exit_status, _code}}, 3_000
    end
  end

  describe "spawn_process/6 CLI mode — argument handling" do
    test "preserves arguments with spaces" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, _pid} =
        Local.spawn_process(
          "t",
          sh,
          ["-c", "echo 'hello world'"],
          "/tmp",
          %{},
          self()
        )

      lines = collect_lines(port, 1, 5_000)
      assert "hello world" in lines

      assert_receive {^port, {:exit_status, 0}}, 5_000
    end

    test "preserves arguments with special characters" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, _pid} =
        Local.spawn_process(
          "t",
          sh,
          ["-c", "echo '$HOME is not expanded'"],
          "/tmp",
          %{},
          self()
        )

      lines = collect_lines(port, 1, 5_000)
      assert "$HOME is not expanded" in lines

      assert_receive {^port, {:exit_status, 0}}, 5_000
    end

    test "preserves arguments with newlines" do
      Application.put_env(:conductor, :dispatch_mode, :cli)

      # Use printf instead of echo to avoid newline interpretation issues
      printf = System.find_executable("printf")

      {:ok, port, _pid} =
        Local.spawn_process(
          "t",
          printf,
          ["line1\nline2\n"],
          "/tmp",
          %{},
          self()
        )

      lines = collect_lines(port, 2, 5_000)
      assert "line1" in lines
      assert "line2" in lines

      assert_receive {^port, {:exit_status, 0}}, 5_000
    end
  end

  describe "spawn_process/6 ACP mode — bidirectional" do
    test "receives exit_status" do
      Application.put_env(:conductor, :dispatch_mode, :acp)
      sh = System.find_executable("sh")

      {:ok, port, _pid} =
        Local.spawn_process("t", sh, ["-c", "exit 0"], "/tmp", %{}, self())

      assert_receive {^port, {:exit_status, 0}}, 5_000
    end

    test "can write to stdin via send_input and read from stdout" do
      Application.put_env(:conductor, :dispatch_mode, :acp)
      sh = System.find_executable("sh")

      # Use a script that reads one line from stdin, echoes it, then exits.
      # This way the process exits naturally (no Port.close needed).
      {:ok, port, _pid} =
        Local.spawn_process(
          "t",
          sh,
          ["-c", "read LINE; echo \"$LINE\""],
          "/tmp",
          %{},
          self()
        )

      # Write to stdin — the script echoes it back then exits
      Local.send_input(port, "hello from conductor\n")

      lines = collect_lines(port, 1, 5_000)
      assert "hello from conductor" in lines

      # Process exits naturally after reading one line
      assert_receive {^port, {:exit_status, 0}}, 5_000
    end

    test "bidirectional JSON-RPC simulation" do
      Application.put_env(:conductor, :dispatch_mode, :acp)
      sh = System.find_executable("sh")

      # Simulate a simple JSON-RPC echo: read a line, echo it back, exit
      {:ok, port, _pid} =
        Local.spawn_process(
          "t",
          sh,
          ["-c", "read LINE; echo \"$LINE\"; exit 0"],
          "/tmp",
          %{},
          self()
        )

      # Write a JSON-RPC message to stdin
      msg = ~s|{"jsonrpc":"2.0","id":1,"method":"test"}|
      Local.send_input(port, msg <> "\n")

      lines = collect_lines(port, 1, 5_000)
      assert msg in lines

      assert_receive {^port, {:exit_status, 0}}, 5_000
    end
  end

  describe "spawn_process/6 — working directory" do
    test "spawns process in the specified working directory" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, _pid} =
        Local.spawn_process("t", sh, ["-c", "pwd"], "/tmp", %{}, self())

      lines = collect_lines(port, 1, 5_000)
      # /tmp may resolve to /private/tmp on macOS
      assert Enum.any?(lines, fn l -> String.contains?(l, "tmp") end)

      assert_receive {^port, {:exit_status, 0}}, 5_000
    end
  end

  describe "stop_process/1" do
    test "closes port — port is no longer alive" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, _os_pid} =
        Local.spawn_process("t", sh, ["-c", "sleep 60"], "/tmp", %{}, self())

      assert Local.alive?(port)

      Local.stop_process(port)

      # Port.close kills the child and closes the port. After close,
      # the port handle is invalid — no more messages arrive. This is
      # by design: the AgentWorker's handle_info for exit_status won't
      # fire. Instead, the AgentWorker uses Port.close as the terminal
      # action (e.g., on timeout), and the GenServer stops via other means.
      refute Local.alive?(port)
    end

    test "is idempotent" do
      Application.put_env(:conductor, :dispatch_mode, :cli)
      sh = System.find_executable("sh")

      {:ok, port, _pid} =
        Local.spawn_process("t", sh, ["-c", "exit 0"], "/tmp", %{}, self())

      assert_receive {^port, {:exit_status, 0}}, 5_000

      # Calling stop on already-closed port should not crash
      assert :ok == Local.stop_process(port)
    end
  end

  # -- Helpers --

  # Collect up to `count` lines from the port within `timeout` ms.
  defp collect_lines(port, count, timeout) do
    deadline = System.monotonic_time(:millisecond) + timeout
    do_collect_lines(port, count, deadline, [])
  end

  defp do_collect_lines(_port, 0, _deadline, acc), do: Enum.reverse(acc)

  defp do_collect_lines(port, remaining, deadline, acc) do
    now = System.monotonic_time(:millisecond)
    wait = max(deadline - now, 0)

    receive do
      {^port, {:data, {:eol, line}}} ->
        do_collect_lines(port, remaining - 1, deadline, [line | acc])

      {^port, {:data, {:noeol, _chunk}}} ->
        # Partial line — keep waiting
        do_collect_lines(port, remaining, deadline, acc)
    after
      wait -> Enum.reverse(acc)
    end
  end
end
