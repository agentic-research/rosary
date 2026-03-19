defmodule Conductor.Provider.Local do
  @moduledoc """
  Local compute provider. Runs agent processes on the conductor's machine
  via Erlang Ports. Supports both ACP and CLI protocols.

  - `provision/3` is a no-op (local machine is always ready)
  - `spawn_process/6` opens an Erlang Port to the agent binary
  - `send_input/2` writes to Port stdin via `Port.command/2`
  - `exec_sync/3` runs a command via `System.cmd/3`
  - `deprovision/1` is a no-op
  """
  @behaviour Conductor.Provider

  require Logger

  @impl true
  def provision(_name, _repo, _opts), do: :ok

  @impl true
  def spawn_process(_name, binary, args, work_dir, _env, _worker_pid) do
    try do
      port =
        Port.open(
          {:spawn_executable, binary},
          [
            :binary,
            :exit_status,
            :out,
            {:line, 65_536},
            args: args,
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
