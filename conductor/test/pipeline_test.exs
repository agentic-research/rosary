defmodule Conductor.PipelineTest do
  use ExUnit.Case, async: true

  alias Conductor.Pipeline
  alias Conductor.Pipeline.Step

  # Tests use explicit pipelines where possible, not templates.
  # Template-dependent tests use Pipeline functions to derive expectations
  # so they don't break when templates change.

  # -- Construction --

  test "for_bead builds pipeline with at least one step" do
    p = Pipeline.for_bead("rsry-abc", "/repo", "bug")
    assert Pipeline.step_count(p) >= 1
    assert p.current == 0
    assert p.bead_id == "rsry-abc"
    assert Pipeline.current_agent(p) == Pipeline.default_agent("bug")
  end

  test "for_bead with current_agent starts at correct phase" do
    p = Pipeline.for_bead("rsry-abc", "/repo", "feature", "staging-agent")
    assert Pipeline.current_agent(p) == "staging-agent"
  end

  test "for_bead unknown type defaults to dev-agent" do
    p = Pipeline.for_bead("rsry-abc", "/repo", "something")
    assert Pipeline.current_agent(p) == "dev-agent"
  end

  # -- Navigation (template-independent: build explicit pipelines) --

  test "current_step returns step struct" do
    p = Pipeline.for_bead("x", "/r", "bug")
    step = Pipeline.current_step(p)
    assert %Step{} = step
    assert is_binary(step.agent)
    assert is_integer(step.timeout_ms)
  end

  test "advance moves to next step" do
    # Build a 2-step pipeline explicitly
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"}), Step.new(%{agent: "b"})],
      current: 0,
      history: []
    }

    assert {:next, p2} = Pipeline.advance(p)
    assert Pipeline.current_agent(p2) == "b"
  end

  test "advance returns :done at end of pipeline" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"})],
      current: 0,
      history: []
    }

    assert :done = Pipeline.advance(p)
  end

  test "advance walks full explicit pipeline" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"}), Step.new(%{agent: "b"}), Step.new(%{agent: "c"})],
      current: 0,
      history: []
    }

    {:next, p} = Pipeline.advance(p)
    assert Pipeline.current_agent(p) == "b"
    {:next, p} = Pipeline.advance(p)
    assert Pipeline.current_agent(p) == "c"
    assert :done = Pipeline.advance(p)
  end

  # -- History + retries --

  test "record adds to history" do
    p =
      %Pipeline{
        bead_id: "x",
        repo: "/r",
        issue_type: "test",
        steps: [Step.new(%{agent: "a"})],
        current: 0,
        history: []
      }
      |> Pipeline.record(:pass, "all good")

    assert length(p.history) == 1
    assert hd(p.history).outcome == :pass
    assert hd(p.history).agent == "a"
  end

  test "retries_used counts failures for current agent" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a", max_retries: 5})],
      current: 0,
      history: []
    }

    assert Pipeline.retries_used(p) == 0
    p = Pipeline.record(p, :fail)
    assert Pipeline.retries_used(p) == 1
    p = Pipeline.record(p, :fail)
    assert Pipeline.retries_used(p) == 2
  end

  test "can_retry? respects max_retries" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a", max_retries: 2})],
      current: 0,
      history: []
    }

    assert Pipeline.can_retry?(p)
    p = Pipeline.record(p, :fail)
    p = Pipeline.record(p, :fail)
    refute Pipeline.can_retry?(p)
  end

  # -- Mutation --

  test "insert_step adds at position" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"})],
      current: 0,
      history: []
    }

    p = Pipeline.insert_step(p, 1, %{agent: "b"})
    assert Pipeline.step_count(p) == 2
    assert Pipeline.agents(p) == ["a", "b"]
  end

  test "insert_step before current adjusts index" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"}), Step.new(%{agent: "b"})],
      current: 1,
      history: []
    }

    p = Pipeline.insert_step(p, 0, %{agent: "z"})
    assert Pipeline.current_agent(p) == "b"
    assert Pipeline.agents(p) == ["z", "a", "b"]
  end

  test "append_step adds at end" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"})],
      current: 0,
      history: []
    }

    p = Pipeline.append_step(p, %{agent: "z"})
    assert List.last(p.steps).agent == "z"
  end

  # -- Query --

  test "done? when past all steps" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"})],
      current: 0,
      history: []
    }

    refute Pipeline.done?(p)
    p = %{p | current: 1}
    assert Pipeline.done?(p)
  end

  test "remaining returns steps after current" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"}), Step.new(%{agent: "b"}), Step.new(%{agent: "c"})],
      current: 0,
      history: []
    }

    remaining = Pipeline.remaining(p)
    assert length(remaining) == 2
    assert hd(remaining).agent == "b"
  end

  test "progress tracks completed phases" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"}), Step.new(%{agent: "b"})],
      current: 0,
      history: []
    }

    assert Pipeline.progress(p) == {0, 2}
    p = Pipeline.record(p, :pass)
    assert Pipeline.progress(p) == {1, 2}
  end

  # -- Serialization --

  test "roundtrip to_map/from_map" do
    p =
      %Pipeline{
        bead_id: "rsry-abc",
        repo: "/repo",
        issue_type: "test",
        steps: [Step.new(%{agent: "a"}), Step.new(%{agent: "b"})],
        current: 0,
        history: []
      }
      |> Pipeline.record(:pass, "phase 1 done")

    map = Pipeline.to_map(p)
    assert map.bead_id == "rsry-abc"
    assert length(map.steps) == 2
    assert length(map.history) == 1

    p2 = Pipeline.from_map(map)
    assert p2.bead_id == "rsry-abc"
    assert Pipeline.agents(p2) == ["a", "b"]
    assert hd(p2.history).outcome == :pass
  end

  # -- Template tests (derive expectations from Pipeline functions) --

  test "default_agent returns first agent in template" do
    assert Pipeline.default_agent("bug") == "dev-agent"
    assert Pipeline.default_agent("review") == "staging-agent"
    assert Pipeline.default_agent("epic") == "pm-agent"
  end

  test "templates have consistent first/next agents" do
    for type <- ["bug", "feature", "task", "chore", "review", "epic", "design", "research"] do
      p = Pipeline.for_bead("x", "/r", type)
      assert Pipeline.step_count(p) >= 1, "#{type} should have at least 1 step"
      assert is_binary(Pipeline.current_agent(p)), "#{type} should have a current agent"
    end
  end

  # -- Step modes --

  test "steps have default mode :implement" do
    step = Step.new(%{agent: "a"})
    assert step.mode == :implement
  end

  test "step modes survive serialization roundtrip" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a"}), Step.new(%{agent: "b", mode: :plan_first})],
      current: 0,
      history: []
    }

    map = Pipeline.to_map(p)
    p2 = Pipeline.from_map(map)
    assert Enum.at(p2.steps, 1).mode == :plan_first
  end

  test "parallel_group survives serialization" do
    p = %Pipeline{
      bead_id: "x",
      repo: "/r",
      issue_type: "test",
      steps: [Step.new(%{agent: "a", parallel_group: :validation})],
      current: 0,
      history: []
    }

    map = Pipeline.to_map(p)
    p2 = Pipeline.from_map(map)
    assert hd(p2.steps).parallel_group == :validation
  end
end
