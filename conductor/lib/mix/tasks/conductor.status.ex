defmodule Mix.Tasks.Conductor.Status do
  @moduledoc """
  Show what the conductor has been doing: active agents, recent dispatches,
  bead state changes, and pipeline progress.

  Usage:
      mix conductor.status              # quick overview
      mix conductor.status --beads      # include recent bead changes from rsry
  """
  use Mix.Task

  @shortdoc "Show conductor status + recent activity"

  @impl Mix.Task
  def run(args) do
    {:ok, _} = Application.ensure_all_started(:conductor)
    show_beads? = "--beads" in args

    IO.puts("Conductor Status")
    IO.puts("================\n")

    # 1. Connection + orchestrator
    status = Conductor.status()
    IO.puts("rsry connected:  #{status.connected}")
    IO.puts("orchestrator:    #{if status.orchestrator, do: "RUNNING", else: "PAUSED"}")
    IO.puts("active agents:   #{status.active_agents}")

    # 2. Active workers
    agents = Conductor.agents()

    if agents != [] do
      IO.puts("\nActive Workers:")

      for a <- agents do
        IO.puts(
          "  #{a.bead_id} | #{a.current_agent} | #{a.progress} | " <>
            "#{a.elapsed_s}s | pid=#{a.os_pid || "?"}"
        )

        if a.title, do: IO.puts("    #{a.title}")

        for h <- a.history || [] do
          IO.puts("    #{h}")
        end
      end
    end

    # 3. Recent bead changes (optional, hits rsry)
    if show_beads? do
      IO.puts("\nRecent Bead Activity (from rsry):")
      show_recent_beads()
    end

    IO.puts("")
  end

  defp show_recent_beads do
    case Conductor.RsryClient.status() do
      {:ok, counts} ->
        IO.puts(
          "  total=#{counts["total"]} open=#{counts["open"]} " <>
            "in_progress=#{counts["in_progress"]} blocked=#{counts["blocked"]}"
        )

      {:error, reason} ->
        IO.puts("  (rsry unreachable: #{inspect(reason)})")
    end

    # Show recently updated beads
    case Conductor.RsryClient.list_beads() do
      {:ok, %{"beads" => beads}} ->
        recent =
          beads
          |> Enum.sort_by(& &1["updated_at"], :desc)
          |> Enum.take(10)

        if recent != [] do
          IO.puts("\n  Last 10 updated:")

          for b <- recent do
            status_icon =
              case b["status"] do
                "closed" -> "[x]"
                "done" -> "[x]"
                "open" -> "[ ]"
                "dispatched" -> "[>]"
                "blocked" -> "[!]"
                _ -> "[?]"
              end

            owner = b["owner"] || "unassigned"
            IO.puts("  #{status_icon} #{b["id"]} #{b["title"]} (#{owner})")
          end
        end

      {:error, _} ->
        :ok
    end
  end
end
