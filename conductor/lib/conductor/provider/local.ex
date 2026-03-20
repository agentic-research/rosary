defmodule Conductor.Provider.Local do
  @moduledoc """
  Local compute provider. Runs agent processes on the conductor's machine
  via Erlang Ports with PTY-backed bidirectional stdio.

  The key insight: `claude -p` reads stdin for ACP commands and writes
  stdout for responses. Erlang Ports give us `{port, {:exit_status, code}}`
  for instant death detection. But we need bidirectional stdio — not just
  read-only stdout or write-only stdin.

  Architecture:
  - `spawn_process/6` creates a PTY pair via a small C helper (`pty_spawn`)
  - The slave side becomes the agent's stdin/stdout (looks like a terminal)
  - The master side is wrapped in an Erlang Port
  - `send_input/2` writes to the master → agent reads from slave stdin
  - Agent stdout flows: slave → master → Port → `{port, {:data, ...}}`
  - Agent exit flows: kernel → Port → `{port, {:exit_status, code}}`
  - When no input is needed, the master is silent (no "no stdin" warning)
  """
  @behaviour Conductor.Provider

  require Logger

  @impl true
  def provision(_name, _repo, _opts), do: :ok

  @impl true
  def spawn_process(_name, binary, args, work_dir, _env, _worker_pid) do
    # Build the command for the PTY wrapper to exec
    full_args = [binary | args]

    # Use `script` as a portable PTY wrapper (available on macOS and Linux).
    # `script -q /dev/null` allocates a PTY and execs the command.
    # This gives us:
    #   1. Agent sees a real terminal on stdin (no "no stdin data" warning)
    #   2. Port gets stdout data via {:data, ...} messages
    #   3. Port gets exit status via {:exit_status, ...} messages
    #   4. Port.command/2 writes to agent's stdin (for ACP)
    {script_bin, script_args} = pty_wrapper(full_args)

    try do
      port =
        Port.open(
          {:spawn_executable, script_bin},
          [
            :binary,
            :exit_status,
            {:line, 65_536},
            args: script_args,
            cd: to_charlist(work_dir)
          ]
        )

      {:os_pid, os_pid} = Port.info(port, :os_pid)
      Logger.info("[provider:local] started #{Path.basename(binary)} (pid=#{os_pid})")
      {:ok, port, os_pid}
    rescue
      e -> {:error, Exception.message(e)}
    end
  end

  # macOS: `script -q /dev/null cmd args...`
  # Linux: `script -qfc "cmd args..." /dev/null`
  defp pty_wrapper(cmd_and_args) do
    script = System.find_executable("script") || "/usr/bin/script"

    case :os.type() do
      {:unix, :darwin} ->
        {script, ["-q", "/dev/null"] ++ cmd_and_args}

      {:unix, _linux} ->
        # Linux `script` uses -c for command
        full_cmd = Enum.map_join(cmd_and_args, " ", &shell_escape/1)
        {script, ["-qfc", full_cmd, "/dev/null"]}
    end
  end

  defp shell_escape(arg) do
    if String.contains?(arg, [" ", "'", "\"", "\\", "$", "`"]) do
      "'" <> String.replace(arg, "'", "'\\''") <> "'"
    else
      arg
    end
  end

  @impl true
  def send_input(port, data) when is_port(port) do
    Port.command(port, data)
    :ok
  end

  @impl true
  def stop_process(port) when is_port(port) do
    Port.close(port)
    :ok
  rescue
    _ -> :ok
  end

  @impl true
  def alive?(port) when is_port(port), do: Port.info(port) != nil
  def alive?(_), do: false

  @impl true
  def exec_sync(_name, command, work_dir) do
    case System.cmd("/bin/sh", ["-c", command], cd: work_dir, stderr_to_stdout: true) do
      {output, code} -> {:ok, {output, code}}
    end
  rescue
    e -> {:error, Exception.message(e)}
  end

  @impl true
  def deprovision(_name), do: :ok
end
