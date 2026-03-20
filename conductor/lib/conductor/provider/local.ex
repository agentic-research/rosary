defmodule Conductor.Provider.Local do
  @moduledoc """
  Local compute provider. Runs agent processes on the conductor's machine
  via Erlang Ports.

  ## Why two spawn paths

  CLI mode (`claude -p "prompt"`):
    The agent gets its prompt from command-line args and does not read stdin.
    However, if stdin is an open pipe with no data, claude CLI warns "no stdin
    data received in 3s." Redirecting stdin from /dev/null (immediate EOF)
    silences this. We use `priv/exec-null-stdin.sh` which does
    `exec "$@" < /dev/null` -- the `exec` replaces the shell so the Port
    monitors the real agent process and receives its exit code.

  ACP mode (bidirectional JSON-RPC):
    The agent speaks ACP over stdin/stdout. The conductor writes JSON-RPC
    requests via `Port.command/2` and reads responses from Port data messages.
    Standard bidirectional Port -- no wrapper needed.

  Both modes preserve:
    - `{port, {:exit_status, code}}` for instant death detection
    - `{port, {:data, {:eol, line}}}` for stdout capture
  """
  @behaviour Conductor.Provider

  require Logger

  @impl true
  def provision(_name, _repo, _opts), do: :ok

  @impl true
  def spawn_process(_name, binary, args, work_dir, _env, _worker_pid) do
    mode = Application.get_env(:conductor, :dispatch_mode, :cli)

    try do
      port = open_port(mode, binary, args, work_dir)

      {:os_pid, os_pid} = Port.info(port, :os_pid)

      Logger.info(
        "[provider:local] started #{Path.basename(binary)} (pid=#{os_pid}, mode=#{mode})"
      )

      {:ok, port, os_pid}
    rescue
      e -> {:error, Exception.message(e)}
    end
  end

  # ACP mode: bidirectional pipes. Conductor writes JSON-RPC to stdin,
  # reads JSON-RPC from stdout. No wrapper needed.
  defp open_port(:acp, binary, args, work_dir) do
    Port.open(
      {:spawn_executable, binary},
      [
        :binary,
        :exit_status,
        {:line, 65_536},
        args: args,
        cd: to_charlist(work_dir)
      ]
    )
  end

  # CLI mode: stdin from /dev/null via exec wrapper.
  #
  # The wrapper script does `exec "$@" < /dev/null` which:
  #   1. Redirects stdin from /dev/null (agent sees EOF, no "no stdin" warning)
  #   2. `exec` replaces the shell process with the agent binary
  #   3. Therefore Port.info(:os_pid) returns the agent's PID (after exec)
  #   4. Port receives the agent's real exit code via {:exit_status, code}
  #   5. Arguments pass through `$@` -- no shell escaping needed
  #
  # stdout remains piped through the Port for --output-format json capture.
  defp open_port(:cli, binary, args, work_dir) do
    wrapper = wrapper_path()

    Port.open(
      {:spawn_executable, wrapper},
      [
        :binary,
        :exit_status,
        {:line, 65_536},
        args: [binary | args],
        cd: to_charlist(work_dir)
      ]
    )
  end

  # Resolve the exec-null-stdin.sh wrapper path.
  # Prefers the priv/ path from the compiled application. Falls back to
  # the source tree path for development / mix test.
  defp wrapper_path do
    priv_path = Application.app_dir(:conductor, "priv/exec-null-stdin.sh")

    if File.exists?(priv_path) do
      priv_path
    else
      # Development fallback: relative to source
      Path.join([__DIR__, "..", "..", "..", "priv", "exec-null-stdin.sh"])
      |> Path.expand()
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
