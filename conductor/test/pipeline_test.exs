defmodule Conductor.PipelineTest do
  use ExUnit.Case, async: true

  alias Conductor.Pipeline
  alias Conductor.Pipeline.Step

  # -- Construction --

  test "for_bead builds pipeline from issue_type" do
    p = Pipeline.for_bead("rsry-abc", "/repo", "bug")
    assert Pipeline.agents(p) == ["dev-agent", "staging-agent"]
    assert p.current == 0
    assert p.bead_id == "rsry-abc"
  end

  test "for_bead with current_agent starts at correct phase" do
    p = Pipeline.for_bead("rsry-abc", "/repo", "feature", "staging-agent")
    assert p.current == 1
    assert Pipeline.current_agent(p) == "staging-agent"
  end

  test "for_bead unknown type defaults to dev-agent" do
    p = Pipeline.for_bead("rsry-abc", "/repo", "something")
    assert Pipeline.agents(p) == ["dev-agent"]
  end

  # -- Navigation --

  test "current_step returns step struct" do
    p = Pipeline.for_bead("x", "/r", "bug")
    step = Pipeline.current_step(p)
    assert %Step{agent: "dev-agent"} = step
    assert step.timeout_ms == 600_000
    assert step.max_retries == 3
  end

  test "current_agent returns agent name" do
    p = Pipeline.for_bead("x", "/r", "bug")
    assert Pipeline.current_agent(p) == "dev-agent"
  end

  test "advance moves to next step" do
    p = Pipeline.for_bead("x", "/r", "bug")
    assert {:next, p2} = Pipeline.advance(p)
    assert Pipeline.current_agent(p2) == "staging-agent"
  end

  test "advance returns :done at end" do
    p = Pipeline.for_bead("x", "/r", "task")
    assert :done = Pipeline.advance(p)
  end

  test "advance through full feature pipeline" do
    p = Pipeline.for_bead("x", "/r", "feature")
    assert Pipeline.current_agent(p) == "dev-agent"

    {:next, p} = Pipeline.advance(p)
    assert Pipeline.current_agent(p) == "staging-agent"

    {:next, p} = Pipeline.advance(p)
    assert Pipeline.current_agent(p) == "prod-agent"

    assert :done = Pipeline.advance(p)
  end

  # -- History + retries --

  test "record adds to history" do
    p = Pipeline.for_bead("x", "/r", "bug") |> Pipeline.record(:pass, "all good")
    assert length(p.history) == 1
    assert hd(p.history).outcome == :pass
    assert hd(p.history).agent == "dev-agent"
  end

  test "retries_used counts failures for current agent" do
    p = Pipeline.for_bead("x", "/r", "bug")
    assert Pipeline.retries_used(p) == 0

    p = Pipeline.record(p, :fail, "exit 1")
    assert Pipeline.retries_used(p) == 1

    p = Pipeline.record(p, :fail, "exit 1")
    assert Pipeline.retries_used(p) == 2
  end

  test "can_retry? respects max_retries" do
    p = Pipeline.for_bead("x", "/r", "bug")
    assert Pipeline.can_retry?(p)

    p = Pipeline.record(p, :fail)
    p = Pipeline.record(p, :fail)
    p = Pipeline.record(p, :fail)
    refute Pipeline.can_retry?(p)
  end

  # -- Mutation --

  test "insert_step adds at position" do
    p = Pipeline.for_bead("x", "/r", "task")
    assert Pipeline.step_count(p) == 1

    p = Pipeline.insert_step(p, 1, %{agent: "staging-agent"})
    assert Pipeline.step_count(p) == 2
    assert Pipeline.agents(p) == ["dev-agent", "staging-agent"]
  end

  test "insert_step before current adjusts index" do
    p = Pipeline.for_bead("x", "/r", "bug")
    {:next, p} = Pipeline.advance(p)
    assert Pipeline.current_agent(p) == "staging-agent"

    # Insert before current — index should shift
    p = Pipeline.insert_step(p, 0, %{agent: "review-agent"})
    assert Pipeline.current_agent(p) == "staging-agent"
    assert Pipeline.agents(p) == ["review-agent", "dev-agent", "staging-agent"]
  end

  test "append_step adds at end" do
    p = Pipeline.for_bead("x", "/r", "task")
    p = Pipeline.append_step(p, %{agent: "prod-agent"})
    assert Pipeline.agents(p) == ["dev-agent", "prod-agent"]
  end

  # -- Query --

  test "done? when past all steps" do
    p = Pipeline.for_bead("x", "/r", "task")
    refute Pipeline.done?(p)

    # Manually set current past end
    p = %{p | current: 1}
    assert Pipeline.done?(p)
  end

  test "remaining returns steps after current" do
    p = Pipeline.for_bead("x", "/r", "feature")
    remaining = Pipeline.remaining(p)
    assert length(remaining) == 2
    assert hd(remaining).agent == "staging-agent"
  end

  test "progress tracks completed phases" do
    p = Pipeline.for_bead("x", "/r", "bug")
    assert Pipeline.progress(p) == {0, 2}

    p = Pipeline.record(p, :pass)
    assert Pipeline.progress(p) == {1, 2}
  end

  # -- Serialization --

  test "roundtrip to_map/from_map" do
    p =
      Pipeline.for_bead("rsry-abc", "/repo", "bug")
      |> Pipeline.record(:pass, "phase 1 done")

    map = Pipeline.to_map(p)
    assert map.bead_id == "rsry-abc"
    assert length(map.steps) == 2
    assert length(map.history) == 1

    p2 = Pipeline.from_map(map)
    assert p2.bead_id == "rsry-abc"
    assert Pipeline.agents(p2) == ["dev-agent", "staging-agent"]
    assert length(p2.history) == 1
    assert hd(p2.history).outcome == :pass
  end

  # -- Backward compat --

  test "default_agent works like Rust version" do
    assert Pipeline.default_agent("bug") == "dev-agent"
    assert Pipeline.default_agent("review") == "staging-agent"
    assert Pipeline.default_agent("epic") == "pm-agent"
  end

  test "next_agent works like Rust version" do
    assert Pipeline.next_agent("bug", "dev-agent") == "staging-agent"
    assert Pipeline.next_agent("bug", "staging-agent") == nil
    assert Pipeline.next_agent("feature", "staging-agent") == "prod-agent"
    assert Pipeline.next_agent("task", "dev-agent") == nil
  end

  # -- Step modes --

  test "steps have default mode :implement" do
    p = Pipeline.for_bead("x", "/r", "bug")
    step = Pipeline.current_step(p)
    assert step.mode == :implement
  end

  test "insert_step with custom mode" do
    p = Pipeline.for_bead("x", "/r", "task")
    p = Pipeline.insert_step(p, 1, %{agent: "review-agent", mode: :plan_first})
    # Inserted after current, so current still points to dev-agent
    assert Pipeline.current_agent(p) == "dev-agent"
    # The inserted step is at index 1
    assert Enum.at(p.steps, 1).agent == "review-agent"
    assert Enum.at(p.steps, 1).mode == :plan_first
  end

  test "step modes survive serialization roundtrip" do
    p = Pipeline.for_bead("x", "/r", "task")
    p = Pipeline.append_step(p, %{agent: "review-agent", mode: :plan_first})

    map = Pipeline.to_map(p)
    p2 = Pipeline.from_map(map)
    review_step = Enum.at(p2.steps, 1)
    assert review_step.mode == :plan_first
  end

  test "step with parallel_group" do
    p = Pipeline.for_bead("x", "/r", "bug")
    p = Pipeline.append_step(p, %{agent: "prod-agent", parallel_group: :review})
    steps = p.steps
    assert List.last(steps).parallel_group == :review
  end

  test "parallel_group survives serialization" do
    p = Pipeline.for_bead("x", "/r", "task")
    p = Pipeline.append_step(p, %{agent: "qa", parallel_group: :validation})
    map = Pipeline.to_map(p)
    p2 = Pipeline.from_map(map)
    assert List.last(p2.steps).parallel_group == :validation
  end
end
