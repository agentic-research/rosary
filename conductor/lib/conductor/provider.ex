defmodule Conductor.Provider do
  @moduledoc """
  Behaviour for compute providers.

  A provider manages WHERE an agent process runs. Protocol (ACP/CLI) and
  model backend (claude/gemini/etc) are orthogonal — both work on any provider.

  ## Providers

  - `Conductor.Provider.Local` — runs agent on the conductor's machine via Erlang Port
  - `Conductor.Provider.Sprites` — runs agent on a remote Sprites VM via WebSocket

  ## Contract

  Every provider must:
  1. Accept `spawn_process/6` returning a handle that sends Port-compatible messages
     (`{handle, {:data, {:eol, line}}}` and `{handle, {:exit_status, code}}`) to the
     given `worker_pid`.
  2. Accept `send_input/2` for writing to the process stdin (needed for ACP JSON-RPC).
  3. Be idempotent on `provision/3` (safe to call on retry).
  4. Be idempotent on `deprovision/1` (safe to call if already destroyed).
  """

  @type handle :: port() | pid()
  @type provider_id :: integer() | String.t()

  @doc "Set up compute environment (create VM, clone repo, etc.). No-op for local."
  @callback provision(name :: String.t(), repo :: String.t(), opts :: map()) ::
              :ok | {:error, term()}

  @doc """
  Start a streaming process that sends Port-compatible messages to `worker_pid`.

  Returns `{:ok, handle, identifier}` where handle is used for `send_input/2`
  and `stop_process/1`, and identifier is for logging (OS PID or sprite name).
  """
  @callback spawn_process(
              name :: String.t(),
              binary :: String.t(),
              args :: [String.t()],
              work_dir :: String.t(),
              env :: map(),
              worker_pid :: pid()
            ) :: {:ok, handle(), provider_id()} | {:error, term()}

  @doc "Send data to process stdin. Used by ACP for JSON-RPC messages."
  @callback send_input(handle(), data :: iodata()) :: :ok | {:error, term()}

  @doc "Stop the running process (close Port or WebSocket)."
  @callback stop_process(handle()) :: :ok

  @doc "Check if the process is still alive."
  @callback alive?(handle()) :: boolean()

  @doc "Run a command synchronously. Used for validation and setup."
  @callback exec_sync(name :: String.t(), command :: String.t(), work_dir :: String.t()) ::
              {:ok, {String.t(), integer()}} | {:error, term()}

  @doc "Tear down compute environment. No-op for local."
  @callback deprovision(name :: String.t()) :: :ok | {:error, term()}

  @doc "Resolve the configured provider module."
  def module do
    Application.get_env(:conductor, :provider_mod) ||
      case Application.get_env(:conductor, :compute_backend, :local) do
        :local -> Conductor.Provider.Local
        :sprites -> Conductor.Provider.Sprites
      end
  end
end
