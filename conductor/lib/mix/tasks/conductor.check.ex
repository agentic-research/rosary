defmodule Mix.Tasks.Conductor.Check do
  @moduledoc """
  Verify the conductor stack is working end-to-end.

  Checks:
  1. ACP adapter binary exists and responds to initialize
  2. rsry HTTP/MCP is reachable
  3. Session creation works (cwd + mcpServers)
  4. (Optional) Full dispatch of a test bead

  Usage:
      mix conductor.check              # check ACP + rsry
      mix conductor.check --dispatch   # also dispatch a test bead
  """
  use Mix.Task

  @shortdoc "Verify conductor stack (ACP + rsry + dispatch)"

  @impl Mix.Task
  def run(args) do
    Application.ensure_all_started(:jason)
    dispatch? = "--dispatch" in args

    IO.puts("Conductor Stack Check")
    IO.puts("=====================\n")

    with :ok <- check_binary(),
         :ok <- check_acp_init(),
         :ok <- check_acp_session(),
         :ok <- check_rsry() do
      IO.puts("\n[OK] All checks passed.")

      if dispatch? do
        check_dispatch()
      end
    else
      {:error, step, reason} ->
        IO.puts("\n[FAIL] #{step}: #{reason}")
        System.halt(1)
    end
  end

  defp check_binary do
    binary = "claude-agent-acp"
    IO.write("1. ACP binary (#{binary})... ")

    case System.find_executable(binary) do
      nil ->
        IO.puts("MISSING")
        IO.puts("   Install: npm install -g @zed-industries/claude-agent-acp")
        {:error, "acp_binary", "#{binary} not found in PATH"}

      path ->
        IO.puts("OK (#{path})")
        :ok
    end
  end

  defp check_acp_init do
    IO.write("2. ACP initialize... ")

    port =
      Port.open(
        {:spawn_executable, System.find_executable("claude-agent-acp")},
        [:binary, :exit_status, {:line, 65_536}]
      )

    msg =
      Jason.encode!(%{
        jsonrpc: "2.0",
        id: 0,
        method: "initialize",
        params: %{
          protocolVersion: 1,
          clientCapabilities: %{fs: %{readTextFile: true}, terminal: true},
          clientInfo: %{name: "conductor-check", version: "0.1.0"}
        }
      })

    Port.command(port, msg <> "\n")

    result =
      receive do
        {^port, {:data, {:eol, line}}} ->
          case Jason.decode(line) do
            {:ok, %{"result" => %{"agentInfo" => info}}} ->
              IO.puts("OK (#{info["name"]} v#{info["version"]})")
              {:ok, port}

            {:ok, %{"error" => err}} ->
              IO.puts("ERROR")
              {:error, "acp_init", inspect(err)}

            _ ->
              IO.puts("unexpected response")
              {:error, "acp_init", "unexpected: #{String.slice(line, 0, 100)}"}
          end

        {^port, {:exit_status, code}} ->
          IO.puts("EXITED (code #{code})")
          {:error, "acp_init", "agent exited with code #{code}"}
      after
        10_000 ->
          IO.puts("TIMEOUT")
          {:error, "acp_init", "no response in 10s"}
      end

    case result do
      {:ok, p} ->
        # Keep port for session test
        Process.put(:check_port, p)
        :ok

      error ->
        Port.close(port)
        error
    end
  end

  defp check_acp_session do
    IO.write("3. ACP session/new... ")
    port = Process.get(:check_port)

    msg =
      Jason.encode!(%{
        jsonrpc: "2.0",
        id: 1,
        method: "session/new",
        params: %{cwd: File.cwd!(), mcpServers: []}
      })

    Port.command(port, msg <> "\n")

    result =
      receive do
        {^port, {:data, {:eol, line}}} ->
          case Jason.decode(line) do
            {:ok, %{"result" => %{"sessionId" => sid}}} ->
              IO.puts("OK (session=#{String.slice(sid, 0, 20)}...)")
              :ok

            {:ok, %{"error" => err}} ->
              IO.puts("ERROR: #{inspect(err)}")
              {:error, "acp_session", inspect(err)}

            _ ->
              IO.puts("unexpected")
              {:error, "acp_session", String.slice(line, 0, 100)}
          end

        {^port, {:exit_status, code}} ->
          IO.puts("EXITED (code #{code})")
          {:error, "acp_session", "exited #{code}"}
      after
        10_000 ->
          IO.puts("TIMEOUT")
          {:error, "acp_session", "timeout"}
      end

    Port.close(port)
    result
  end

  defp check_rsry do
    IO.write("4. rsry MCP (HTTP)... ")
    url = Application.get_env(:conductor, :rsry_url, "http://127.0.0.1:8383/mcp")

    Application.ensure_all_started(:req)

    body =
      Jason.encode!(%{
        jsonrpc: "2.0",
        id: 1,
        method: "initialize",
        params: %{
          protocolVersion: "2024-11-05",
          capabilities: %{},
          clientInfo: %{name: "conductor-check", version: "0.1.0"}
        }
      })

    case Req.post(url,
           json: Jason.decode!(body),
           headers: [
             {"accept", "application/json, text/event-stream"},
             {"content-type", "application/json"}
           ]
         ) do
      {:ok, %{status: 200}} ->
        IO.puts("OK (#{url})")
        :ok

      {:ok, resp} ->
        IO.puts("HTTP #{resp.status}")
        {:error, "rsry", "HTTP #{resp.status}"}

      {:error, reason} ->
        IO.puts("UNREACHABLE")
        IO.puts("   Start: rsry serve --transport http --port 8383")
        {:error, "rsry", inspect(reason)}
    end
  end

  defp check_dispatch do
    IO.puts("\n5. Test dispatch...")
    IO.puts("   (not yet implemented — needs full app start)")

    IO.puts(
      "   Run: iex -S mix -e 'Conductor.dispatch(\"test-bead\", \".\", issue_type: \"task\")'"
    )
  end
end
