# Motion audit

Per-frame traces of every element in the example apps, and a checker that flags motion a user
would notice: snaps, pops, neighbours that jump, exits that vanish before they play, overshoot,
motion that never settles, and work spent on things nobody can see.

```sh
scripts/motion-audit/run.sh                        # record all scenarios into target/motion-audit
python3 scripts/motion-audit/analyze.py target/motion-audit/*.jsonl [--quiet] [--json out.json]
python3 scripts/motion-audit/track.py target/motion-audit/oc-send.jsonl composer/editor 80 120
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
| `jump`, `step`, `sibling-jump` | a one-frame change in position or size, on its own or next to something animating |
| `overshoot`, `jiggle` | passes its target, or reverses direction more than once |
| `latency`, `hitch` | input with no motion for several frames; a frame over budget |

`track.py` prints one element's frames (rect, opacity, flags, sampled tracks) to confirm a
finding. Add scenarios as script JSON under `scenarios/` and a `run` line in `run.sh`.
