#!/usr/bin/env python3
"""Summarize an OpenCode run transcript: tool calls, files touched, final report."""
import json
import sys

def main(path):
    tools, texts = [], []
    for line in open(path):
        if not line.startswith("{"):
            continue
        try:
            part = json.loads(line).get("part", {})
        except ValueError:
            continue
        if part.get("type") == "tool":
            state = part.get("state", {})
            arg = state.get("input", {})
            label = arg.get("command") or arg.get("filePath") or str(arg)
            tools.append((part.get("tool"), label[:100], state.get("status")))
        elif part.get("type") == "text":
            texts.append(part.get("text", ""))
    stalled = sum(1 for t in tools if t[2] != "completed")
    print(f"{len(tools)} tool calls; {stalled} not completed")
    for tool, label, status in tools:
        if tool in ("write", "edit") or status != "completed":
            print("  ", tool, label, status)
    print("=== FINAL REPORT ===")
    print(texts[-1] if texts else "(no final text)")

if __name__ == "__main__":
    main(sys.argv[1])
