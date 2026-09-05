#!/usr/bin/env python3
"""Predicates over captured live rows; never produces or injects provider rows."""
import json
import pathlib
import sys


def blocks(row, kind):
    content = row.get("message", {}).get("content", [])
    return [b for b in content if isinstance(b, dict) and b.get("type") == kind]


def matches(row, predicate, args):
    kind = row.get("type")
    if predicate == "ready":
        return kind == "amux.claude_sdk.ready"
    if predicate == "mode":
        return kind == "amux.claude_sdk.session_facts" and row.get("permission_mode") == args[0]
    if predicate == "stream":
        event = row.get("event", {})
        return (kind == "stream_event" and event.get("type") == "content_block_delta"
                and event.get("delta", {}).get("type") == "text_delta")
    if predicate == "result":
        return kind == "result" and row.get("subtype") == "success" and not row.get("is_error")
    if predicate == "interrupted":
        return kind == "result" and row.get("terminal_reason") == "aborted_streaming"
    if predicate == "permission":
        return kind == "amux.claude_sdk.permission_required" and row.get("tool_name") == args[0]
    if predicate == "elicitation":
        return kind == "amux.claude_sdk.elicitation_required" and row.get("server") == "external"
    if predicate == "resolved":
        return (kind == "amux.claude_sdk." + args[0] + "_resolved"
                and row.get("request_id") == args[1] and row.get("decision") == args[2])
    if predicate == "assistant":
        return kind == "assistant" and any(args[0] in b.get("text", "") for b in blocks(row, "text"))
    if predicate == "prompt":
        return kind == "user" and row.get("input_id") and row.get("message", {}).get("content") == args[0]
    if predicate == "tool-result":
        return kind == "user" and any(
            not b.get("is_error") and args[0] in json.dumps(b.get("content"))
            for b in blocks(row, "tool_result"))
    if predicate == "message":
        envelope = row.get("envelope", {})
        return (kind == "amux.claude_sdk.message" and envelope.get("kind") == "message"
                and envelope.get("text") == args[0] and envelope.get("from", {}).get("name") == args[1])
    if predicate == "completed":
        envelope = row.get("envelope", {})
        return (kind == "amux.claude_sdk.message" and envelope.get("kind") == "completed"
                and envelope.get("from", {}).get("name") == args[0])
    raise ValueError("unknown row predicate: " + predicate)


def main():
    path, since, predicate, *args = sys.argv[1:]
    rows = [json.loads(line) for line in pathlib.Path(path).read_text().splitlines()]
    if predicate == "cursor":
        print(rows[-1]["seq"] if rows else -1)
        return
    for entry in rows:
        if entry["seq"] > int(since) and matches(entry["payload"], predicate, args):
            if predicate == "interrupted" and not any(
                int(since) < earlier["seq"] < entry["seq"]
                and earlier["payload"].get("type") == "user"
                and any(b.get("text") == "[Request interrupted by user]"
                        for b in blocks(earlier["payload"], "text"))
                for earlier in rows
            ):
                continue
            if predicate in ("permission", "elicitation"):
                print(entry["payload"]["request_id"])
            else:
                print(entry["seq"])
            return
    sys.exit(1)


if __name__ == "__main__":
    main()
