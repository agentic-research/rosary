defmodule Conductor.ProviderTest do
  use ExUnit.Case, async: true

  alias Conductor.Provider

  describe "module/0" do
    test "returns Local by default" do
      Application.delete_env(:conductor, :provider_mod)
      Application.put_env(:conductor, :compute_backend, :local)

      assert Provider.module() == Conductor.Provider.Local
    end

    test "returns Sprites when compute_backend is :sprites" do
      Application.delete_env(:conductor, :provider_mod)
      Application.put_env(:conductor, :compute_backend, :sprites)

      assert Provider.module() == Conductor.Provider.Sprites

      # Reset
      Application.put_env(:conductor, :compute_backend, :local)
    end

    test "respects provider_mod override" do
      Application.put_env(:conductor, :provider_mod, MyCustomProvider)

      assert Provider.module() == MyCustomProvider

      Application.delete_env(:conductor, :provider_mod)
    end
  end
end
