"""Guard against false live-acceptance positives from echoes and unrelated asks."""
import unittest

from sdk_chat_rows import matches


class LiveRowPredicates(unittest.TestCase):
    def test_instructions_and_completion_are_not_an_exchanged_message(self):
        envelope = {"kind": "message", "text": "When you receive PING, reply PONG", "from": {"name": "parent"}}
        row = {"type": "amux.claude_sdk.message", "envelope": envelope}
        self.assertFalse(matches(row, "message", ["PING", "parent"]))
        envelope["text"] = "PING"
        self.assertTrue(matches(row, "message", ["PING", "parent"]))
        self.assertFalse(matches(row, "message", ["PING", "other"]))
        envelope["kind"] = "completed"
        self.assertFalse(matches(row, "message", ["PING", "parent"]))

    def test_prompt_echo_is_not_a_reply(self):
        row = {"type": "user", "message": {"content": [{"type": "text", "text": "DONE"}]}}
        self.assertFalse(matches(row, "assistant", ["DONE"]))
        row["type"] = "assistant"
        self.assertTrue(matches(row, "assistant", ["DONE"]))

    def test_resolution_requires_the_request_and_decision(self):
        row = {"type": "amux.claude_sdk.permission_resolved", "request_id": "one", "decision": "allow"}
        self.assertTrue(matches(row, "resolved", ["permission", "one", "allow"]))
        self.assertFalse(matches(row, "resolved", ["permission", "two", "allow"]))
        self.assertFalse(matches(row, "resolved", ["permission", "one", "deny"]))
        self.assertFalse(matches(row, "resolved", ["elicitation", "one", "allow"]))

    def test_failed_tool_result_is_not_continuation(self):
        block = {"type": "tool_result", "content": "HERON", "is_error": True}
        row = {"type": "user", "message": {"content": [block]}}
        self.assertFalse(matches(row, "tool-result", ["HERON"]))
        block["is_error"] = False
        self.assertTrue(matches(row, "tool-result", ["HERON"]))

    def test_stream_requires_partial_text(self):
        row = {"type": "stream_event", "event": {"type": "content_block_delta",
               "delta": {"type": "input_json_delta", "partial_json": "{}"}}}
        self.assertFalse(matches(row, "stream", []))
        row["event"]["delta"] = {"type": "text_delta", "text": "Trees"}
        self.assertTrue(matches(row, "stream", []))

    def test_error_is_not_successful_turn(self):
        row = {"type": "result", "subtype": "success", "is_error": True}
        self.assertFalse(matches(row, "result", []))
        row["is_error"] = False
        self.assertTrue(matches(row, "result", []))


if __name__ == "__main__":
    unittest.main()
