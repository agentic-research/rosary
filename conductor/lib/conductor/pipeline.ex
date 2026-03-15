defmodule Conductor.Pipeline do
  @moduledoc """
  Agent pipeline — pure functions for phase progression.

  Mirrors dispatch.rs agent_pipeline/default_agent/next_agent.
  """

  @pipelines %{
    "bug" => ["dev-agent", "staging-agent"],
    "feature" => ["dev-agent", "staging-agent", "prod-agent"],
    "task" => ["dev-agent"],
    "chore" => ["dev-agent"],
    "review" => ["staging-agent"],
    "epic" => ["pm-agent"],
    "design" => ["pm-agent"],
    "research" => ["pm-agent"]
  }

  @doc "The agent pipeline for a given issue type."
  def pipeline(issue_type) do
    Map.get(@pipelines, issue_type, ["dev-agent"])
  end

  @doc "The default (first) agent for a given issue type."
  def default_agent(issue_type) do
    pipeline(issue_type) |> List.first("dev-agent")
  end

  @doc "The next agent after `current`, or nil if pipeline complete."
  def next_agent(issue_type, current) do
    agents = pipeline(issue_type)

    case Enum.find_index(agents, &(&1 == current)) do
      nil -> nil
      idx -> Enum.at(agents, idx + 1)
    end
  end

  @doc "Is this the final phase?"
  def final_phase?(issue_type, current) do
    next_agent(issue_type, current) == nil
  end

  @doc "Total number of phases for an issue type."
  def phase_count(issue_type), do: length(pipeline(issue_type))
end
