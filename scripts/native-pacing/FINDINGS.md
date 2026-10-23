# Refresh-rate investigation, October 23, 2026

Rune's tested overlay does **not** present at 120 Hz with its default synced
layer. Both CVDisplayLink and NSView CADisplayLink deliver about 120 callbacks/s,
while positive Metal presentations remain about 60/s. Changing callback API
alone does not close the gap.

All runs used Pro, sequential source-hash-checked HiveMind jobs, CAS sync off,
and `/tmp/pro-gui.lock`. No capture. Endpoint: `nextDrawable` **entry** to positive
Metal `presentedTime`; two-second warmup, three three-second intervals per batch,
three batches. Latency below is the median of nine interval medians. Hz is the
range across batches, not an inference from callback frequency.

| Experiment | Positive presentation Hz | Latency ms | Short acquisition gaps |
| --- | ---: | ---: | ---: |
| Rebased production, CV / 3 drawables / sync on | 59.94–60.06 | 68.830 | 0 |
| NSView CA, preferred 80–120 / 3 / sync on | 59.22–59.89 | 66.136 | 0 |
| CV / 2 drawables / sync on | 59.22–60.06 | 44.177 | 0 |
| CV / 3 / sync off | 106.50–109.42 | 6.613 | 0 |
| Instrumented guard control, CV / 3 / sync on | 52.54–57.88 | 61.492 | 0 |
| Same binary, half-refresh guard disabled | 53.21–58.09 | 72.279 | 16 |

## Findings in the requested order

1. **CADisplayLink:** macOS 14+ NSView display links follow the view's display and
   stop callbacks when hidden/off-display. Preferred frame-rate ranges are
   best-effort. The adapter delivered 116.5–119.2 callbacks/s but about 60 positive
   presentations/s. This rules out the CV callback rate alone as the limit in
   this fixture; it does not evaluate a fully native CA scheduler's lifecycle.
2. **Layer settings:** two drawables reduce queue depth/latency but retain 60-Hz
   presentation. Disabling sync changes the result substantially: median drawable
   wait falls to 0.015–0.017 ms and positive presentations rise above 100/s. That
   is not proof of tear-free 120-Hz output. Apple documents possible tearing with
   `displaySyncEnabled = false`; these timestamp-only runs cannot assess it.
3. **Half-refresh guard:** the control counts 537–686 short-spacing candidates
   and 729–798 admissions per full run. Removing that guard alone shifts waiting
   into `nextDrawable`: median wait grows from 4.4–4.7 ms to 15.8–16.2 ms, without
   increasing presentation frequency. Sixteen acquisition gaps violate the
   4.167-ms half-refresh bound; minimum 0.570 ms. Keep the guard.

The evidence points to synced drawable/presentation backpressure rather than
insufficient display-link callbacks. It does not yet identify why this layer's
synced path releases drawables near 60 Hz on a reported 120-Hz display. No
production scheduling or layer changes are retained from this investigation.
The lower-risk candidate for a separate implementation is two drawable buffers;
it still needs an actual wgpu configuration change and lifecycle verification.

## Evidence and reproducibility

Production source: `bf634d517656c29402713a945841769d416ef1b8`.
Production fixture SHA-256:
`16e1cdd0afc23eb31cf2402225b65aaeed783df749e35d04bbd80622677dd3d3`.
Diagnostic admission fixture SHA-256:
`92469348aea776b18e721fc27c0f1c987f6dc99be0dc9e44ac4066fe7c3433ed`.
The only Rust difference is the retained, unapplied `admission-probe.patch`.

Local evidence in `artifacts/scrawl-gaps/`:
- `steady-rebased/`, `refresh-ca/`, `refresh-two-drawables/`, `refresh-unsynced/`.
- `refresh-guard-control/`, `refresh-no-half/`.
- `rebased-source.sha256`, `refresh-source.sha256`, `guard-source.sha256`.

Each directory retains raw logs, summary JSON, and before/after host load.
The guard batches ran under more variable callback delivery (about 104–113 Hz)
than the earlier layer experiments, so compare guard-off against its own
instrumented control, not against the earlier 68.830-ms baseline. The control
and guard-off rates overlap; no 120-Hz improvement was observed.

Rune's layer is 1360×320 pixels; the Scrawl reference is 1280×168. Zero Metal
timestamps are excluded, not counted as dropped frames. The unsynced runs had
9–11 excluded timestamps each. Callback counts span the full run; reported
presentation rates span only the selected sample. No screenshot or physical
scanout measurement was taken.

Primary sources:
- [NSView displayLink](https://developer.apple.com/documentation/appkit/nsview/displaylink(target:selector:))
- [preferredFrameRateRange](https://developer.apple.com/documentation/quartzcore/cadisplaylink/preferredframeraterange)
- [displaySyncEnabled](https://developer.apple.com/documentation/quartzcore/cametallayer/displaysyncenabled)
