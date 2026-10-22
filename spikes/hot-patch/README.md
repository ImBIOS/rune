# Headless Subsecond spike

Measured on Pro on 2026-10-21. See [the research report](../../research/08-spike-hot-patch.md) for conclusions and qualifications. `evidence/` contains the recorded JSON results; `results/` contains disposable full logs from a run.

This is a Rust terminal binary, with no Dioxus UI or window. `main` owns the state and runs a nominal 120 Hz loop. The patched function returns a string. The Python driver edits Rust source and consumes actual frame events, using timestamps from the same machine.

## Run on Pro

Do not run Cargo on Studio.

```sh
ssh pro 'mkdir -p ~/scratch/rune-hot-patch; git -C ~/scratch/rune-hot-patch init'
rsync -az --exclude target --exclude results --exclude evidence \
  ~/code/open-source/rune/spikes/hot-patch/ pro:scratch/rune-hot-patch/
```

Open a shell on Pro and set its tool paths:

```sh
ssh pro
export PATH="$HOME/.cargo/bin:$HOME/.local/bin:$HOME/bin:/opt/homebrew/bin:$PATH"
```

If Dioxus CLI is missing, install the release used by this spike:

```sh
hivemind run --worker pro --kind cargo \
  --dir "$HOME/scratch/rune-hot-patch" --no-sync -- \
  cargo-binstall dioxus-cli --version 0.7.10 --no-confirm
"$HOME/.cargo/bin/dx" --version
```

`/opt/homebrew/bin/dx` was Deno's command, not Dioxus. The harness deliberately uses the absolute Dioxus path.

Run each mode under a lease:

```sh
for mode in builds benchmark probes extras; do
  hivemind run --worker pro --kind cargo \
    --dir "$HOME/scratch/rune-hot-patch" --no-sync -- \
    python3 "$HOME/scratch/rune-hot-patch/run.py" "$mode"
done
```

`builds` requires a fresh artifact directory; choose a new `RUNE_NORMAL_TARGET` if `target/normal` already exists. This prevents labeling a warm build as cold. For example, put `env RUNE_NORMAL_TARGET=target/normal-new` before `python3` in the command above. Each probe launches a fresh process, patches once, records its observations, stops the process group, and restores the source. Do not run modes concurrently.

The boundary-signature probe can be run alone:

```sh
hivemind run --worker pro --kind cargo \
  --dir "$HOME/scratch/rune-hot-patch" --no-sync -- \
  python3 "$HOME/scratch/rune-hot-patch/run.py" extras --case explicit_hotfn_signature
```

The underlying headless command is:

```sh
~/.cargo/bin/dx serve --hot-patch --platform server \
  --open false --interactive false --addr 127.0.0.1 --port 18743
```

The harness disables the worker's inherited sccache wrapper, enables incremental compilation, and limits Cargo to two jobs. It retains HiveMind's background scheduling policy. A nominal 120 Hz sleep loop runs in bursts under that policy; this is not a display-pacing benchmark.

## Interpreting the probes

- The benchmark uses a two-reference closure capture to work around 0.7.10's pointer-sized-closure dispatch bug. `extras` reproduces the broken single capture and verifies `HotFn::current(render).call((&state,))` as a cleaner integration.
- The layout probe adds a field but never explicitly reads it. It demonstrates that patches can cross incompatible layouts without a rejection or migration; it does not establish memory safety.
- A successful revision with unchanged PID, increasing counter, unchanged state/Vec addresses, and `vec0 == counter + 73` establishes preservation of this sample's main-owned state only.
- Private and exported tip-crate statics, thread locals, and cached trait objects have separate observations. Their behavior must not be inferred from the main-owned-state result.
- `p95_nearest_rank_ms` is the largest of ten samples. There is no statistical claim beyond this small run.
