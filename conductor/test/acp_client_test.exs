defmodule Conductor.AcpClientTest do
  use ExUnit.Case, async: true

  alias Conductor.AcpClient

  describe "policy_allows?/2" do
    test "read_only allows read tools and MCP" do
      assert AcpClient.policy_allows?("Read", :read_only)
      assert AcpClient.policy_allows?("Glob", :read_only)
      assert AcpClient.policy_allows?("Grep", :read_only)
      assert AcpClient.policy_allows?("mcp__mache__search", :read_only)
      assert AcpClient.policy_allows?("mcp__rsry__status", :read_only)
    end

    test "read_only rejects write tools" do
      refute AcpClient.policy_allows?("Edit", :read_only)
      refute AcpClient.policy_allows?("Write", :read_only)
      refute AcpClient.policy_allows?("Bash(cargo test)", :read_only)
    end

    test "implement allows read + write + bash + MCP" do
      assert AcpClient.policy_allows?("Read", :implement)
      assert AcpClient.policy_allows?("Edit", :implement)
      assert AcpClient.policy_allows?("Write", :implement)
      assert AcpClient.policy_allows?("Bash(cargo test)", :implement)
      assert AcpClient.policy_allows?("mcp__rsry__bead_close", :implement)
    end

    test "plan allows read + MCP only" do
      assert AcpClient.policy_allows?("Read", :plan)
      assert AcpClient.policy_allows?("mcp__mache__get_overview", :plan)
      refute AcpClient.policy_allows?("Edit", :plan)
      refute AcpClient.policy_allows?("Bash(cargo test)", :plan)
    end
  end

  describe "policy_for/2" do
    test "issue_type mapping" do
      assert AcpClient.policy_for("bug") == :implement
      assert AcpClient.policy_for("task") == :implement
      assert AcpClient.policy_for("feature") == :implement
      assert AcpClient.policy_for("review") == :read_only
      assert AcpClient.policy_for("epic") == :plan
      assert AcpClient.policy_for("design") == :plan
      assert AcpClient.policy_for("research") == :plan
    end

    test "step mode overrides issue_type" do
      # :plan_first forces read_only regardless of issue_type
      assert AcpClient.policy_for("bug", :plan_first) == :read_only
      assert AcpClient.policy_for("feature", :plan_first) == :read_only

      # :read_only forces read_only
      assert AcpClient.policy_for("bug", :read_only) == :read_only

      # :implement defers to issue_type
      assert AcpClient.policy_for("bug", :implement) == :implement
      assert AcpClient.policy_for("review", :implement) == :read_only
    end
  end

  describe "handle_message/1" do
    test "parses permission request" do
      msg =
        Jason.encode!(%{
          "jsonrpc" => "2.0",
          "id" => 42,
          "method" => "session/request_permission",
          "params" => %{
            "toolCall" => %{"toolCallId" => "tc-1", "fields" => %{"title" => "Edit"}},
            "options" => [
              %{"optionId" => "allow-once", "name" => "Allow", "kind" => "allow_once"},
              %{"optionId" => "reject-once", "name" => "Reject", "kind" => "reject_once"}
            ]
          }
        })

      assert {:ok, {:permission_request, 42, tool_call, options}} = AcpClient.handle_message(msg)
      assert tool_call["toolCallId"] == "tc-1"
      assert length(options) == 2
    end

    test "parses tool_call update" do
      msg =
        Jason.encode!(%{
          "jsonrpc" => "2.0",
          "method" => "session/update",
          "params" => %{
            "update" => %{
              "sessionUpdate" => "tool_call",
              "toolCallId" => "tc-1",
              "title" => "Edit",
              "kind" => "file_edit"
            }
          }
        })

      assert {:ok, {:tool_call, "tc-1", "Edit", "file_edit"}} = AcpClient.handle_message(msg)
    end

    test "parses prompt completion" do
      msg =
        Jason.encode!(%{
          "jsonrpc" => "2.0",
          "id" => 2,
          "result" => %{"stopReason" => "end_turn", "cost" => 0.05}
        })

      assert {:ok, {:prompt_complete, "end_turn", _result}} = AcpClient.handle_message(msg)
    end

    test "handles malformed JSON gracefully" do
      assert {:ok, {:unknown, "not json at all"}} = AcpClient.handle_message("not json at all")
    end

    test "handles unknown messages" do
      msg = Jason.encode!(%{"something" => "unexpected"})
      assert {:ok, {:unknown, _}} = AcpClient.handle_message(msg)
    end
  end
end
