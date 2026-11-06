import { invoke } from "@tauri-apps/api/core";
import type { McpTestRequest } from "../../generated/McpTestRequest";
import type { McpTestResultView } from "../../generated/McpTestResultView";

/** Starts a test only when called with approval from the panel's Run test action. */
export function testMcpConnection(
  request: Omit<McpTestRequest, "user_approved">,
  explicitApproval: true,
): Promise<McpTestResultView> {
  return invoke<McpTestResultView>("mcp_test_connection", { request: { ...request, user_approved: explicitApproval } });
}
