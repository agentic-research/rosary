defmodule Conductor.PipelineTest do
  use ExUnit.Case, async: true

  alias Conductor.Pipeline

  test "bug goes through dev then staging" do
    assert Pipeline.pipeline("bug") == ["dev-agent", "staging-agent"]
  end

  test "feature has three phases" do
    assert Pipeline.pipeline("feature") == ["dev-agent", "staging-agent", "prod-agent"]
  end

  test "task is dev only" do
    assert Pipeline.pipeline("task") == ["dev-agent"]
  end

  test "epic is pm only" do
    assert Pipeline.pipeline("epic") == ["pm-agent"]
  end

  test "unknown defaults to dev" do
    assert Pipeline.pipeline("something") == ["dev-agent"]
  end

  test "default_agent returns first in pipeline" do
    assert Pipeline.default_agent("bug") == "dev-agent"
    assert Pipeline.default_agent("review") == "staging-agent"
    assert Pipeline.default_agent("epic") == "pm-agent"
  end

  test "next_agent advances pipeline" do
    assert Pipeline.next_agent("bug", "dev-agent") == "staging-agent"
    assert Pipeline.next_agent("bug", "staging-agent") == nil
  end

  test "next_agent feature full pipeline" do
    assert Pipeline.next_agent("feature", "dev-agent") == "staging-agent"
    assert Pipeline.next_agent("feature", "staging-agent") == "prod-agent"
    assert Pipeline.next_agent("feature", "prod-agent") == nil
  end

  test "next_agent unknown current returns nil" do
    assert Pipeline.next_agent("bug", "unknown") == nil
  end

  test "final_phase? detects end" do
    assert Pipeline.final_phase?("task", "dev-agent")
    refute Pipeline.final_phase?("bug", "dev-agent")
    assert Pipeline.final_phase?("bug", "staging-agent")
  end

  test "phase_count" do
    assert Pipeline.phase_count("bug") == 2
    assert Pipeline.phase_count("feature") == 3
    assert Pipeline.phase_count("task") == 1
  end
end
