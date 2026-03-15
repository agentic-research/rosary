defmodule Conductor.RepoResolver do
  @moduledoc """
  Resolves repo names to absolute paths.

  Beads store repo as a short name (e.g., "rosary", "mache").
  The conductor needs absolute paths for Port.open cd:.

  Resolution order:
  1. If already absolute (starts with /), use as-is
  2. Check configured repo map
  3. Check common locations (~/ prefixed paths from rsry config)
  4. Fall back to current directory
  """

  @doc "Resolve a repo name or path to an absolute path."
  def resolve(repo) when is_binary(repo) do
    cond do
      String.starts_with?(repo, "/") ->
        repo

      path = configured_path(repo) ->
        path

      path = discover_path(repo) ->
        path

      true ->
        # Last resort: assume CWD
        Path.join(File.cwd!(), repo)
    end
  end

  defp configured_path(repo) do
    repos = Application.get_env(:conductor, :repos, %{})
    Map.get(repos, repo)
  end

  @common_prefixes [
    "~/remotes/art",
    "~/github/art",
    "~/github/jamestexas",
    "~/remotes"
  ]

  defp discover_path(repo) do
    home = System.user_home!()

    @common_prefixes
    |> Enum.map(fn prefix ->
      prefix
      |> String.replace("~", home)
      |> Path.join(repo)
    end)
    |> Enum.find(&File.dir?/1)
  end
end
