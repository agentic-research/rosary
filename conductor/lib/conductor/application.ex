defmodule Conductor.Application do
  @moduledoc """
  OTP Application — starts the conductor supervision tree.

  Tree: RsryClient → AgentSupervisor → Orchestrator
  (in order, so client is ready before orchestrator ticks)
  """
  use Application

  @impl true
  def start(_type, _args) do
    children =
      [
        Conductor.RsryClient,
        Conductor.AgentSupervisor,
        Conductor.Orchestrator
      ] ++ sprites_children()

    opts = [strategy: :one_for_one, name: Conductor.Supervisor]
    Supervisor.start_link(children, opts)
  end

  defp sprites_children do
    if Application.get_env(:conductor, :compute_backend) == :sprites do
      [Conductor.SpritesSweeper]
    else
      []
    end
  end
end
