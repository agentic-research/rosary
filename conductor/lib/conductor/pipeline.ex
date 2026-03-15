defmodule Conductor.Pipeline do
  @moduledoc """
  First-class agent pipeline — the agent path as a closure.

  A Pipeline is a data structure that captures the full execution plan
  for a bead: which agents run in what order, with what behavior on
  success/failure, and what context is carried between phases.

  The pipeline is the continuation — it knows where we are, where we're
  going, and what to do when things go wrong. It survives crashes by
  being serializable to/from Dolt.

  ## Design

  Each step is a closure-like struct: it captures the agent name,
  timeout, max retries, and callbacks for success/failure. The pipeline
  walks through steps sequentially. On success, it advances. On failure,
  it retries or escalates.

  Unlike the Rust implementation (static match arms + index arithmetic),
  pipelines here are runtime values that can be:
  - Inspected by the PM agent
  - Modified at runtime (insert a review step)
  - Persisted to Dolt and recovered after crashes
  - Composed from templates + bead-specific overrides
  """

  alias __MODULE__.Step

  defstruct [
    :bead_id,
    :repo,
    :issue_type,
    steps: [],
    current: 0,
    history: []
  ]

  @type t :: %__MODULE__{
          bead_id: String.t(),
          repo: String.t(),
          issue_type: String.t(),
          steps: [Step.t()],
          current: non_neg_integer(),
          history: [history_entry()]
        }

  @type history_entry :: %{
          step: non_neg_integer(),
          agent: String.t(),
          outcome: :pass | :fail | :timeout | :skip,
          timestamp: DateTime.t(),
          detail: String.t() | nil
        }

  # -- Templates: issue_type → default pipeline --

  @validation_implement %{command: "task test", interval_ms: 300_000, on_fail: :notify_agent}
  @validation_review %{command: "task test", interval_ms: 300_000, on_fail: :kill}

  @templates %{
    "bug" => [
      %{
        agent: "dev-agent",
        timeout_ms: 600_000,
        max_retries: 3,
        validation: @validation_implement
      },
      %{
        agent: "staging-agent",
        timeout_ms: 600_000,
        max_retries: 2,
        validation: @validation_review
      }
    ],
    "feature" => [
      %{
        agent: "dev-agent",
        timeout_ms: 600_000,
        max_retries: 3,
        validation: @validation_implement
      },
      %{
        agent: "staging-agent",
        timeout_ms: 600_000,
        max_retries: 2,
        validation: @validation_review
      },
      %{agent: "prod-agent", timeout_ms: 600_000, max_retries: 2, validation: @validation_review}
    ],
    "task" => [
      %{
        agent: "dev-agent",
        timeout_ms: 600_000,
        max_retries: 3,
        validation: @validation_implement
      }
    ],
    "chore" => [
      %{
        agent: "dev-agent",
        timeout_ms: 600_000,
        max_retries: 3,
        validation: @validation_implement
      }
    ],
    "review" => [
      %{agent: "staging-agent", timeout_ms: 600_000, max_retries: 2}
    ],
    "epic" => [
      %{agent: "pm-agent", timeout_ms: 900_000, max_retries: 2}
    ],
    "design" => [
      %{agent: "pm-agent", timeout_ms: 900_000, max_retries: 2}
    ],
    "research" => [
      %{agent: "pm-agent", timeout_ms: 900_000, max_retries: 2}
    ]
  }

  # -- Construction --

  @doc """
  Build a pipeline for a bead from its issue_type.

  Uses the template for the issue type, falling back to a single
  dev-agent step for unknown types.
  """
  def for_bead(bead_id, repo, issue_type) do
    step_defs =
      Map.get(@templates, issue_type, [%{agent: "dev-agent", timeout_ms: 600_000, max_retries: 3}])

    steps = Enum.map(step_defs, &Step.new/1)

    %__MODULE__{
      bead_id: bead_id,
      repo: repo,
      issue_type: issue_type,
      steps: steps,
      current: 0,
      history: []
    }
  end

  @doc "Build a pipeline starting from a specific agent (for beads mid-flight)."
  def for_bead(bead_id, repo, issue_type, current_agent) do
    pipeline = for_bead(bead_id, repo, issue_type)
    idx = Enum.find_index(pipeline.steps, &(&1.agent == current_agent)) || 0
    %{pipeline | current: idx}
  end

  # -- Navigation --

  @doc "The current step, or nil if pipeline is exhausted."
  def current_step(%__MODULE__{steps: steps, current: idx}) do
    Enum.at(steps, idx)
  end

  @doc "The current agent name."
  def current_agent(pipeline) do
    case current_step(pipeline) do
      nil -> nil
      step -> step.agent
    end
  end

  @doc "Advance to the next step. Returns {:next, pipeline} or :done."
  def advance(%__MODULE__{} = pipeline) do
    next_idx = pipeline.current + 1

    if next_idx < length(pipeline.steps) do
      {:next, %{pipeline | current: next_idx}}
    else
      :done
    end
  end

  @doc "Record a phase outcome in history."
  def record(%__MODULE__{} = pipeline, outcome, detail \\ nil) do
    entry = %{
      step: pipeline.current,
      agent: current_agent(pipeline),
      outcome: outcome,
      timestamp: DateTime.utc_now(),
      detail: detail
    }

    %{pipeline | history: pipeline.history ++ [entry]}
  end

  # -- Retry logic --

  @doc "How many retries have been used for the current step."
  def retries_used(%__MODULE__{} = pipeline) do
    agent = current_agent(pipeline)

    pipeline.history
    |> Enum.count(&(&1.agent == agent and &1.outcome == :fail))
  end

  @doc "Can the current step be retried?"
  def can_retry?(%__MODULE__{} = pipeline) do
    case current_step(pipeline) do
      nil -> false
      step -> retries_used(pipeline) < step.max_retries
    end
  end

  # -- Mutation --

  @doc "Insert a step at a given position."
  def insert_step(%__MODULE__{} = pipeline, position, step_def) do
    step = Step.new(step_def)
    steps = List.insert_at(pipeline.steps, position, step)

    # Adjust current index if we inserted before it
    current =
      if position <= pipeline.current,
        do: pipeline.current + 1,
        else: pipeline.current

    %{pipeline | steps: steps, current: current}
  end

  @doc "Append a step at the end."
  def append_step(%__MODULE__{} = pipeline, step_def) do
    step = Step.new(step_def)
    %{pipeline | steps: pipeline.steps ++ [step]}
  end

  # -- Query --

  @doc "Is the pipeline complete?"
  def done?(%__MODULE__{steps: steps, current: idx}), do: idx >= length(steps)

  @doc "Remaining steps after current."
  def remaining(%__MODULE__{steps: steps, current: idx}) do
    Enum.drop(steps, idx + 1)
  end

  @doc "All agent names in order."
  def agents(%__MODULE__{steps: steps}), do: Enum.map(steps, & &1.agent)

  @doc "Total number of steps."
  def step_count(%__MODULE__{steps: steps}), do: length(steps)

  @doc "Progress as {completed, total}."
  def progress(%__MODULE__{} = pipeline) do
    completed =
      pipeline.history
      |> Enum.filter(&(&1.outcome == :pass))
      |> Enum.map(& &1.step)
      |> Enum.uniq()
      |> length()

    {completed, step_count(pipeline)}
  end

  # -- Serialization (for Dolt persistence) --

  @doc "Serialize pipeline to a JSON-compatible map."
  def to_map(%__MODULE__{} = p) do
    %{
      bead_id: p.bead_id,
      repo: p.repo,
      issue_type: p.issue_type,
      steps: Enum.map(p.steps, &Step.to_map/1),
      current: p.current,
      history:
        Enum.map(p.history, fn h ->
          %{
            step: h.step,
            agent: h.agent,
            outcome: to_string(h.outcome),
            timestamp: DateTime.to_iso8601(h.timestamp),
            detail: h.detail
          }
        end)
    }
  end

  @doc "Deserialize pipeline from a map (handles both atom and string keys)."
  def from_map(map) do
    get = fn m, k -> Map.get(m, k) || Map.get(m, to_string(k)) end

    %__MODULE__{
      bead_id: get.(map, :bead_id),
      repo: get.(map, :repo),
      issue_type: get.(map, :issue_type),
      steps: Enum.map(get.(map, :steps) || [], &Step.from_map/1),
      current: get.(map, :current) || 0,
      history:
        Enum.map(get.(map, :history) || [], fn h ->
          %{
            step: get.(h, :step),
            agent: get.(h, :agent),
            outcome: get.(h, :outcome) |> to_string() |> String.to_existing_atom(),
            timestamp: get.(h, :timestamp) |> parse_timestamp(),
            detail: get.(h, :detail)
          }
        end)
    }
  end

  defp parse_timestamp(%DateTime{} = dt), do: dt
  defp parse_timestamp(s) when is_binary(s), do: DateTime.from_iso8601(s) |> elem(1)

  # -- Convenience (backward compat with simple API) --

  @doc "The default (first) agent for a given issue type."
  def default_agent(issue_type) do
    template = Map.get(@templates, issue_type, [%{agent: "dev-agent"}])
    hd(template).agent
  end

  @doc "The next agent after `current` for a given issue type."
  def next_agent(issue_type, current) do
    template = Map.get(@templates, issue_type, [%{agent: "dev-agent"}])
    agents = Enum.map(template, & &1.agent)

    case Enum.find_index(agents, &(&1 == current)) do
      nil -> nil
      idx -> Enum.at(agents, idx + 1)
    end
  end
end

defmodule Conductor.Pipeline.Step do
  @moduledoc """
  A single step in an agent pipeline.

  ## Modes

  - `:implement` (default) — agent has read/write permissions, does the work
  - `:plan_first` — agent plans in read-only mode, then implements after approval
  - `:read_only` — agent can only read/analyze (for review, audit)

  ## Parallel groups

  Steps with the same `parallel_group` are dispatched simultaneously.
  Steps without a group run sequentially. Groups complete when all
  members finish, then the next sequential step (or group) begins.
  """

  defstruct [
    :agent,
    timeout_ms: 600_000,
    max_retries: 3,
    mode: :implement,
    parallel_group: nil,
    validation: nil
  ]

  @type validation :: %{
          command: String.t(),
          interval_ms: non_neg_integer(),
          on_fail: :notify_agent | :kill | :log_only
        }

  @type mode :: :implement | :plan_first | :read_only
  @type t :: %__MODULE__{
          agent: String.t(),
          timeout_ms: non_neg_integer(),
          max_retries: non_neg_integer(),
          mode: mode(),
          parallel_group: atom() | nil,
          validation: validation() | nil
        }

  def new(attrs) when is_map(attrs) do
    %__MODULE__{
      agent: attrs[:agent] || attrs["agent"],
      timeout_ms: attrs[:timeout_ms] || attrs["timeout_ms"] || 600_000,
      max_retries: attrs[:max_retries] || attrs["max_retries"] || 3,
      mode: parse_mode(attrs[:mode] || attrs["mode"]),
      parallel_group: parse_atom(attrs[:parallel_group] || attrs["parallel_group"]),
      validation: parse_validation(attrs[:validation] || attrs["validation"])
    }
  end

  def to_map(%__MODULE__{} = s) do
    %{
      agent: s.agent,
      timeout_ms: s.timeout_ms,
      max_retries: s.max_retries,
      mode: to_string(s.mode),
      parallel_group: if(s.parallel_group, do: to_string(s.parallel_group)),
      validation: s.validation
    }
  end

  def from_map(map), do: new(map)

  defp parse_mode(nil), do: :implement
  defp parse_mode(m) when is_atom(m), do: m
  defp parse_mode(m) when is_binary(m), do: String.to_existing_atom(m)

  defp parse_atom(nil), do: nil
  defp parse_atom(a) when is_atom(a), do: a
  defp parse_atom(a) when is_binary(a), do: String.to_atom(a)

  defp parse_validation(nil), do: nil

  defp parse_validation(%{} = v) do
    %{
      command: v[:command] || v["command"],
      interval_ms: v[:interval_ms] || v["interval_ms"] || 300_000,
      on_fail: parse_on_fail(v[:on_fail] || v["on_fail"])
    }
  end

  defp parse_on_fail(nil), do: :notify_agent
  defp parse_on_fail(f) when is_atom(f), do: f
  defp parse_on_fail(f) when is_binary(f), do: String.to_existing_atom(f)
end
