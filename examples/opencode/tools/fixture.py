#!/usr/bin/env python3
"""Regenerates assets/fixture.json: a synthetic OpenCode server state in the API's own shapes.

usage: tools/fixture.py > assets/fixture.json

Everything here is invented (projects, sessions, transcript). The streaming turn is a list of
/api/event payloads with times, played back by the app from the moment the session opens.
"""
import json
import random

rng = random.Random(7)
NOW = 1792659400000
MIN = 60_000
HOUR = 60 * MIN
DAY = 24 * HOUR

MODEL = {"id": "claude-opus-5-5", "providerID": "anthropic", "variant": "high"}

projects = [
    ("lumen", "/Users/demo/code/lumen", 2 * MIN),
    ("harbor", "/Users/demo/code/harbor", 40 * MIN),
    ("tidepool", "/Users/demo/code/tidepool", 3 * HOUR),
    ("quill", "/Users/demo/code/quill", 26 * HOUR),
    ("atlas-cli", "/Users/demo/code/atlas-cli", 3 * DAY),
    ("kiln", "/Users/demo/code/kiln", 6 * DAY),
]


def pid(name):
    return name.encode().hex()[:16].ljust(16, "0")



out_projects = [
    {"id": pid(n), "canonical": d, "vcs": "git", "time": {"created": NOW - 30 * DAY, "updated": NOW - a, "active": NOW - a}, "sandboxes": []}
    for n, d, a in projects
]

sessions = []
counter = [0]


def sid():
    counter[0] += 1
    return f"ses_fx{counter[0]:04d}" + "a" * 18


def session(project, title, age, cost=0.42, tokens=48_000, parent=None):
    s = {
        "id": sid(),
        "projectID": pid(project),
        "agent": "build",
        "model": MODEL,
        "cost": cost,
        "tokens": {"input": tokens // 10, "output": tokens // 12, "reasoning": 900, "cache": {"read": tokens, "write": 2400}},
        "time": {"created": NOW - age - 20 * MIN, "updated": NOW - age},
        "title": title,
        "location": {"directory": dict((n, d) for n, d, _ in projects)[project]},
    }
    if parent:
        s["parentID"] = parent
    sessions.append(s)
    return s["id"]


main = session("lumen", "Fix flaky reconnect in the event stream", 1 * MIN, cost=1.84, tokens=61_400)
review = session("lumen", "Review the reconnect change", 4 * MIN, cost=0.31, tokens=22_000, parent=main)
stale = session("lumen", "Sidebar shows stale session titles", 35 * MIN)
shortcuts = session("lumen", "Add keyboard shortcuts to the command palette", 3 * HOUR)
session("lumen", "Profile scroll jank in long transcripts", 27 * HOUR)
session("harbor", "Migrate config loader to the new schema", 40 * MIN)
session("harbor", "Retry uploads after a dropped connection", 5 * HOUR)
session("tidepool", "Explain the tile cache eviction policy", 3 * HOUR)
session("tidepool", "Write a benchmark for the merge step", 2 * DAY)
session("quill", "Fix footnote numbering in exported PDFs", 26 * HOUR)
session("atlas-cli", "Add --json output to the status command", 3 * DAY)
session("kiln", "Upgrade the build to the 2026 toolchain", 6 * DAY)

mid = [0]


def msg_id(t):
    mid[0] += 1
    return f"msg_{t:013x}{mid[0]:03d}fixture00"


def user(t, text):
    return {"id": msg_id(t), "type": "user", "text": text, "time": {"created": t}}


def tool(name, status, inp, t):
    return {
        "type": "tool",
        "id": f"call_{name}_{t % 100000}",
        "name": name,
        "executed": True,
        "state": {"status": status, "input": inp},
        "time": {"created": t, "completed": t + 900},
    }


def assistant(t, content, dur=6000, finish="stop", cost=0.12, ctx=52_000):
    return {
        "id": msg_id(t),
        "type": "assistant",
        "agent": "build",
        "model": MODEL,
        "content": content,
        "time": {"created": t, "streamed": t + dur - 200, "completed": t + dur},
        "finish": finish,
        "cost": cost,
        "tokens": {"input": 4, "output": 620, "reasoning": 180, "cache": {"read": ctx, "write": 1800}},
    }


T0 = NOW - 18 * MIN
transcript = [
    user(T0, "The event stream drops after the laptop sleeps, and the sidebar stops updating until I reload. Can you find out why and fix it?"),
    assistant(
        T0 + 2000,
        [
            {"type": "reasoning", "text": "The client opens one EventSource and never checks whether it is still alive. After sleep the socket is half-open: no error fires, so nothing reconnects. I should look at the stream client and the heartbeat handling.", "time": {"created": T0 + 2100, "completed": T0 + 6100}},
            {"type": "text", "text": "I'll start with how the client opens the stream and what it does when it goes quiet."},
            tool("grep", "completed", {"pattern": "EventSource|heartbeat"}, T0 + 7000),
            tool("read", "completed", {"filePath": "/Users/demo/code/lumen/src/net/stream.ts"}, T0 + 8000),
            tool("read", "completed", {"filePath": "/Users/demo/code/lumen/src/net/backoff.ts"}, T0 + 9000),
        ],
        dur=10_000,
        finish="tool-calls",
    ),
    assistant(
        T0 + 14_000,
        [
            {
                "type": "text",
                "text": (
                    "## What's happening\n\n"
                    "The server sends a heartbeat every **10 seconds**, but `stream.ts` only reconnects on an `error` event. "
                    "When the machine sleeps, the TCP connection goes half-open: the browser never sees an error, so the client waits forever.\n\n"
                    "Two smaller problems make it worse:\n\n"
                    "- `backoff.ts` resets its delay on *every* message, so a flapping server is retried at full speed.\n"
                    "- The sidebar store ignores `session.updated` events that arrive before the first list fetch.\n\n"
                    "### The fix\n\n"
                    "Treat silence as failure. If no event or heartbeat arrives for 25 seconds, close the stream and reconnect with backoff:\n\n"
                    "```ts\n"
                    "const QUIET_MS = 25_000\n\n"
                    "function watchdog(stream: EventStream, reconnect: () => void) {\n"
                    "  let timer = setTimeout(fire, QUIET_MS)\n"
                    "  function fire() {\n"
                    "    stream.close()\n"
                    "    reconnect()\n"
                    "  }\n"
                    "  stream.on(\"message\", () => {\n"
                    "    clearTimeout(timer)\n"
                    "    timer = setTimeout(fire, QUIET_MS)\n"
                    "  })\n"
                    "}\n"
                    "```\n\n"
                    "I also moved the backoff reset to after a stream has stayed healthy for a minute."
                ),
            },
            tool("edit", "completed", {"filePath": "/Users/demo/code/lumen/src/net/stream.ts"}, T0 + 30_000),
            tool("edit", "completed", {"filePath": "/Users/demo/code/lumen/src/net/backoff.ts"}, T0 + 31_000),
            tool("shell", "completed", {"command": "bun test src/net", "description": "Run the network tests"}, T0 + 33_000),
        ],
        dur=24_000,
        finish="tool-calls",
    ),
    assistant(
        T0 + 40_000,
        [
            tool("task", "completed", {"description": "Review the reconnect change", "subagent_type": "general", "prompt": "Review the diff in src/net for correctness."}, T0 + 40_500),
        ],
        dur=90_000,
        finish="tool-calls",
    ),
    assistant(
        T0 + 131_000,
        [
            {
                "type": "text",
                "text": (
                    "Done. The stream now recovers from sleep on its own.\n\n"
                    "1. **Watchdog**: `stream.ts` closes and reopens the stream after 25 s of silence.\n"
                    "2. **Backoff**: the delay only resets after a minute of healthy traffic.\n"
                    "3. **Sidebar**: early `session.updated` events are queued until the list loads.\n\n"
                    "The reviewer agreed and suggested one change, which I applied: the watchdog now also restarts on `visibilitychange`, so waking the laptop reconnects immediately instead of after 25 s. "
                    "All 14 tests in `src/net` pass."
                ),
            }
        ],
        dur=7000,
        cost=0.21,
        ctx=61_400,
    ),
    user(T0 + 9 * MIN, "Nice. Can you add a test for the backoff, so the reset can't regress?"),
]

review_msgs = [
    user(T0 + 41_000, "Review the diff in src/net for correctness."),
    assistant(T0 + 42_000, [{"type": "text", "text": "The watchdog is correct. One suggestion: also restart on `visibilitychange`."}], dur=8000),
]

T1 = NOW - 50 * MIN
stale_msgs = [
    user(T1, "After I rename a session, the sidebar keeps the old title until I restart. Why?"),
    assistant(
        T1 + 2000,
        [
            tool("grep", "completed", {"pattern": "session.renamed"}, T1 + 2500),
            tool("read", "completed", {"filePath": "/Users/demo/code/lumen/src/sidebar/store.ts"}, T1 + 3000),
            {"type": "text", "text": (
                "The sidebar store copies each session's title when the list loads and never listens for `session.renamed`.\n\n"
                "I added the handler and a test:\n\n"
                "```ts\n"
                "events.on(\"session.renamed\", ({ sessionID, title }) => {\n"
                "  store.update(sessionID, (s) => ({ ...s, title }))\n"
                "})\n"
                "```\n\n"
                "Renames now show up in the sidebar within a frame."
            )},
        ],
        dur=9000,
    ),
]

T2 = NOW - 3 * HOUR - 30 * MIN
shortcut_msgs = [
    user(T2, "Add shortcuts for opening the command palette and jumping between sessions."),
    assistant(T2 + 3000, [{"type": "text", "text": (
        "Added three shortcuts:\n\n"
        "- **⌘K** opens the command palette.\n"
        "- **⌘[** and **⌘]** move to the previous and next session.\n\n"
        "They're registered in `keymap.ts` and listed in the help panel."
    )}], dur=8000),
]

# The streaming turn, as /api/event payloads.
reply_id = msg_id(T0 + 9 * MIN + 1500)
reply_text = (
    "I'll add a test that drives the backoff with a fake clock, so it can check both the growth and the reset rule.\n\n"
    "The interesting cases are:\n\n"
    "- a flapping server that fails right after each message, which must keep backing off;\n"
    "- a stream that stays healthy for a minute, which resets the delay;\n"
    "- the cap, so retries never wait longer than 30 seconds.\n\n"
    "```ts\n"
    "import { expect, test } from \"bun:test\"\n"
    "import { Backoff } from \"./backoff\"\n\n"
    "test(\"flapping keeps backing off\", () => {\n"
    "  const clock = fakeClock()\n"
    "  const b = new Backoff({ clock, base: 1000, cap: 30_000 })\n"
    "  const delays = [1, 2, 3, 4].map(() => {\n"
    "    b.message()\n"
    "    return b.fail()\n"
    "  })\n"
    "  expect(delays).toEqual([1000, 2000, 4000, 8000])\n"
    "})\n"
    "```\n\n"
    "With the old reset-on-every-message rule this test fails on the second delay, which is exactly the regression you were worried about. "
    "The healthy-minute case uses the same clock and advances it by 61 seconds before the next failure."
)

events = []
t = 0.25


def ev(ty, **data):
    events.append({"t": round(t, 3), "type": ty, "data": {"sessionID": main, "assistantMessageID": reply_id, **data}})


ev("session.step.started", agent="build", model=MODEL, started=NOW)
t += 0.35
ev("session.reasoning.started", ordinal=0)
thought = "They want a regression test for the reset rule. A fake clock keeps it deterministic; cover flapping, the healthy minute, and the cap."
words = thought.split(" ")
for k in range(0, len(words), 3):
    t += 0.06
    ev("session.reasoning.delta", ordinal=0, delta=" ".join(words[k : k + 3]) + " ")
t += 0.2
ev("session.reasoning.ended", ordinal=0, text=thought)
t += 0.25
ev("session.tool.input.started", id="call_read_reply", name="read")
t += 0.15
ev("session.tool.called", id="call_read_reply", input={"filePath": "/Users/demo/code/lumen/src/net/backoff.ts"}, executed=True)
t += 0.45
ev("session.tool.success", id="call_read_reply", content=[{"type": "text", "text": "…"}], executed=True)
t += 0.3
ev("session.text.started", ordinal=2)
# Stream like a model: one to three words per delta, uneven gaps, faster inside code.
pieces = []
buf = ""
for ch in reply_text:
    buf += ch
    if ch in " \n" and rng.random() < 0.55:
        pieces.append(buf)
        buf = ""
if buf:
    pieces.append(buf)
in_code = False
for p in pieces:
    if "```" in p:
        in_code = not in_code
    t += (0.018 if in_code else 0.032) + rng.random() * (0.03 if in_code else 0.05)
    ev("session.text.delta", ordinal=2, delta=p)
t += 0.2
ev("session.text.ended", ordinal=2, text=reply_text)
t += 0.1
ev("session.step.ended", finish="stop", cost=0.18, tokens={"input": 6, "output": 540, "reasoning": 120, "cache": {"read": 63_200, "write": 900}})

# A short reply for the mock send path ($MSG is replaced per send).
reply = []
t = 0.5
for ty, data in [
    ("session.step.started", {"agent": "build", "model": MODEL}),
    ("session.text.started", {"ordinal": 0}),
]:
    reply.append({"t": round(t, 3), "type": ty, "data": data})
    t += 0.3
for w in "Got it. I'll take a look and report back with what I find.".split(" "):
    t += 0.05
    reply.append({"t": round(t, 3), "type": "session.text.delta", "data": {"ordinal": 0, "delta": w + " "}})
t += 0.2
reply.append({"t": round(t, 3), "type": "session.step.ended", "data": {"finish": "stop", "cost": 0.01}})

json.dump(
    {
        "now": NOW,
        "projects": out_projects,
        "sessions": sorted(sessions, key=lambda s: -s["time"]["updated"]),
        "messages": {main: transcript, review: review_msgs, stale: stale_msgs, shortcuts: shortcut_msgs},
        "open": main,
        "stream": {"session": main, "events": events},
        "reply": reply,
    },
    __import__("sys").stdout,
    indent=1,
)
print()
