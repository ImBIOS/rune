# Motion audit

Per-frame traces of every element in the example apps, and a checker that flags motion a user
would notice: snaps, pops, neighbours that jump, exits that vanish before they play, overshoot,
motion that never settles, and work spent on things nobody can see.

```sh
scripts/motion-audit/run.sh                        # record all scenarios into target/motion-audit
python3 scripts/motion-audit/analyze.py target/motion-audit/*.jsonl [--quiet] [--json out.json]
python3 scripts/motion-audit/track.py target/motion-audit/oc-send.jsonl composer/editor 80 120
python3 scripts/motion-audit/test_analyze.py        # analyze.py on synthetic traces
```

`run.sh` records each scenario headless at 120 fps on the virtual clock with
`--trace-all --trace`. `--trace-all` adds unkeyed elements (named by path) to the usual
motion-lint JSONL, and every trace now says why each frame animated (`quality.why`) and which
tracks it sampled (`tk`: `[element id or path, property, active]`; inactive means settling).

`analyze.py` reports, per trace:

| Finding | Meaning |
| --- | --- |
| `invisible-work` | tracks sampled on an element that is transparent, off-window, clipped or occluded |
| `idle-frames`, `never-settles` | frames that animate with no visible change; motion still running at the end |
| `pop-in`, `pop-out` | an element appears or disappears at full opacity and size with no transition |
| `appear-then-jump` | mounts in one place, then moves there in a frame |
| `jump`, `step`, `sibling-jump` | a one-frame change in position or size, on its own or next to something animating, seen on both frames (a loop wrapping while transparent or clipped is not one) |
| `overshoot`, `jiggle` | passes its target, or reverses direction more than once |
| `latency`, `hitch` | input with no motion for several frames; a frame over budget |
| `cut-exit` | unmounts partway through its fade: nothing leaves above opacity 0 |
| `reversal`, `dog-leg` | one motion heads the wrong way first or goes out and back; a route much longer than its chord |

An app's scenario can say what each input should do with `"expect"` on its script event:

| Expect | Checks | Finding |
| --- | --- | --- |
| `instant` | everything in `scope` (an element name part) lands within a frame or two of the input; pops and jumps there are intended | `not-instant` |
| `follow` | a drag's `element` moves 1:1 with the pointer while it moves (it may stop at a limit) | `lags-hand` |
| `smooth` | after the input, `element` travels over several frames, not in one | `snap` |
| `leaves` | `element` is what leaves, not a sibling (index keys remove the last row) | `wrong-exit` |

```sh
python3 scripts/motion-audit/analyze.py --script s.json trace.jsonl --allow allow.json --fail
```

`--allow` takes a JSON list of `{"trace", "kind" (or a list), "element" (a substring),
"reason"}` rules for intended behaviour; every rule needs a reason, and a rule for a trace
the run recorded that matched nothing is listed as unused. `--fail` exits non-zero when a finding isn't allowed.

A motion that hides what it moves (a fade to 0, a collapse, a slide out) runs on for a few
frames below what anyone can see, down to its settle threshold. Up to 250 ms of that tail,
ending as its track settles, isn't `invisible-work`. A loop (a spinner) runs for as long as
it's shown, so frames animating only loops aren't `idle-frames` or `never-settles`.

Elements that draw nothing and hold nothing (hit and hover zones, spacers) carry the trace's
blank flag (64). Nobody can see them come, go or move, so they don't pop, jump or count as
drawn.

`track.py` prints one element's frames (rect, opacity, flags, sampled tracks) to confirm a
finding. Add scenarios as script JSON under `scenarios/` and a `run` line in `run.sh`.
