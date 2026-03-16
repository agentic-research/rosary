defmodule Mix.Tasks.Conductor.Run do
  @moduledoc """
  Start the conductor as a long-running daemon.

  Boots the full OTP tree, configures file logging, unpauses the
  orchestrator, and blocks so the BEAM stays alive until interrupted.

  Designed for overnight / unattended operation.

  ## Usage

      mix conductor.run
      mix conductor.run --log /tmp/conductor.log
      mix conductor.run --interval 60000 --max 5
      mix conductor.run --log conductor.log --interval 15000 --max 2

  ## Options

    * `--log`      — Path to log file. When set, logger output goes to
                     this file instead of the console. Default: console only.
    * `--interval` — Scan interval in milliseconds. Overrides
                     `:scan_interval_ms` from config. Default: 30000.
    * `--max`      — Maximum concurrent agents. Overrides
                     `:max_concurrent` from config. Default: 3.
  """
  use Mix.Task

  @shortdoc "Start conductor daemon (overnight dispatch)"

  @switches [log: :string, interval: :integer, max: :integer, repo: :string]

  @doc false
  def parse_args(args) do
    {parsed, _rest, _invalid} = OptionParser.parse(args, strict: @switches)

    %{
      log: Keyword.get(parsed, :log),
      interval: Keyword.get(parsed, :interval),
      max: Keyword.get(parsed, :max),
      repo: Keyword.get(parsed, :repo)
    }
  end

  @impl Mix.Task
  def run(args) do
    opts = parse_args(args)

    # Apply config overrides BEFORE starting the application.
    # auto_start tells the Orchestrator to begin dispatching immediately.
    Application.put_env(:conductor, :auto_start, true)

    if opts.repo do
      Application.put_env(:conductor, :repo_filter, opts.repo)
    end

    if opts.interval do
      Application.put_env(:conductor, :scan_interval_ms, opts.interval)
    end

    if opts.max do
      Application.put_env(:conductor, :max_concurrent, opts.max)
    end

    # Configure file logging before app start so the handler is in place
    # when the supervision tree boots.
    if opts.log do
      configure_file_logging(opts.log)
    end

    # Start the full OTP tree (RsryClient -> AgentSupervisor -> Orchestrator).
    # Orchestrator will auto-start because we set :auto_start above.
    {:ok, _} = Application.ensure_all_started(:conductor)

    # Read back effective config (may have been set by config files if no CLI override)
    interval = Application.get_env(:conductor, :scan_interval_ms, 30_000)
    max_concurrent = Application.get_env(:conductor, :max_concurrent, 3)

    repo_filter = Application.get_env(:conductor, :repo_filter)
    print_banner(opts.log, interval, max_concurrent, repo_filter)

    # Block forever — the BEAM stays up until Ctrl-C / SIGTERM
    Process.sleep(:infinity)
  end

  defp configure_file_logging(path) do
    # Ensure the directory exists
    dir = Path.dirname(path)

    if dir != "." do
      File.mkdir_p!(dir)
    end

    # Add an OTP :logger file handler. This writes structured log messages
    # to the specified file. We keep the default console handler as well
    # so interactive users still see output.
    handler_config = %{
      config: %{
        file: String.to_charlist(path),
        max_no_bytes: 10_485_760,
        max_no_files: 5
      },
      formatter:
        {:logger_formatter,
         %{
           template: [:time, " [", :level, "] ", :msg, "\n"],
           single_line: true
         }}
    }

    :logger.add_handler(:conductor_file, :logger_std_h, handler_config)
  end

  defp print_banner(log_path, interval, max_concurrent, repo_filter) do
    pid = System.pid()

    IO.puts("""

    ========================================
      Conductor Daemon
    ========================================
      PID:        #{pid}
      Log:        #{log_path || "(console)"}
      Interval:   #{interval}ms
      Max agents: #{max_concurrent}
      Repo:       #{repo_filter || "(all repos)"}
    ----------------------------------------
      Ctrl-C to stop.
    ========================================
    """)
  end
end
