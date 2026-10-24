# 16. Agent-native and generative UI: what Rune should steal

Scope: LLMs generating UI at runtime, how agents perceive and operate UIs, token-efficient UI representations, LLM
codegen failure modes for UI frameworks, self-verifying loops, design-system-constrained generation. Today is 2026-10-21.
Evidence quality is marked per item: **[read]** = I read the primary source this session; **[recall]** = from memory of the
paper/post, not re-verified now, treat numbers as approximate.

## Summary

- Every serious generative-UI system converged on the same shape: **a closed catalog of components, a flat
  id-referenced element list (adjacency list), a separate data model, and a streamed patch/message protocol.** Google's A2UI,
  Vercel's json-render, and Airbnb-style server-driven UI all look like this. Rune's command channel (tree diff + targets +
  text runs) is already the same shape, one level lower. Expose it, don't build a second system.
- Two safety models exist: **declarative data against a trusted catalog** (A2UI, json-render: no code runs) and **sandboxed
  code** (MCP Apps / OpenAI Apps SDK: HTML in a locked iframe). For a native-feel renderer, declarative is the only option that
  gets real springs, native text and a11y. Code runs only in a separate, capability-limited process (WASM), later.
- Models are good at this now but not at first-try correctness of *novel framework APIs*. Failure evidence points at
  spatial/structural errors (missing elements, wrong size/position) and at strict type/trait semantics in Rust. Both are
  answered by the same fix: a small closed vocabulary, a schema the decoder is constrained to, and a validator that returns
  fixes.
- Agents operate UIs best from a **semantic tree plus screenshots**, with element ids in both (set-of-marks style). Rune owns the
  tree, so it can emit a compact, stable-id, diffable text form that no web or native framework can match.
- The biggest wedge is not "LLM makes the UI at runtime" but **"the agent's design-time loop is a first-class product":**
  scene → render → critique → patch, with exact lint results, so first-try correctness comes from the loop, not the prompt.

## Findings (ranked roughly by value to Rune)

### 1. Catalog + flat element list + data model + streaming messages (A2UI)  → **adopt now (as Rune's own "Surface" protocol, A2UI-compatible)**
- Source: [A2UI v0.9.1 spec](https://a2ui.org/specification/v0.9.1-a2ui/), [v1.0 candidate](https://github.com/a2ui-project/a2ui/blob/main/specification/v1_0/docs/a2ui_protocol.md) (Jun 2026), [Google launch post](https://developers.googleblog.com/introducing-a2ui-an-open-project-for-agent-driven-interfaces/) (Dec 2025). **[read]**
- What it does: four messages (`createSurface`, `updateComponents`, `updateDataModel`, `deleteSurface`). Components are a flat
  list with `id`s and child id references, so the model can emit them in any order and the client renders progressively
  (placeholders for missing children). Properties bind to a JSON-Pointer data model or to catalog-declared functions
  (`formatCurrency`, `pluralize`, `required`…). Catalogs are JSON Schema documents with an optional `instructions` markdown
  field that is fed to the model. User actions go back as `action` messages; `sendDataModel` returns the surface state with each.
- Rune use: define Rune's runtime-UI surface as A2UI-shaped (flat elements, ids, data model, patch messages) with the **Rune
  design system as the catalog** (Stack, Text, Button, List, Sheet, Glass…). Map `updateComponents` onto the existing keyed
  `El` tree diff, so identity-preserving enter/exit and springs come free: a model that re-sends a component with the same id and
  changed props gets a retargeted spring, not a pop. A2UI has no motion vocabulary; Rune's catalog adds `transition` and
  `layout` tokens (named, from the theme), never raw durations.
- Cost/risk: low. It is JSON, Rune already needs a serializable channel. Risk is spec churn (v0.8→0.9→1.0 renamed
  everything in 6 months) so keep an adapter layer, don't make A2UI the internal format.
- Verdict: **adopt now.** Design the internal wire format first (see §Recommendations), ship an A2UI adapter second.

### 2. Constrain decoding to the catalog schema (XGrammar and structured outputs)  → **adopt now**
- Source: [XGrammar, MLSys'25](https://arxiv.org/abs/2411.15100): up to 100x faster CFG-constrained decoding, near-zero
  overhead end-to-end in serving. **[read]** Provider-side structured outputs (OpenAI/Anthropic/Gemini JSON-schema modes) **[recall]**.
- What it does: the model cannot emit an unknown component, a misspelled prop, or an enum value outside the set. Syntax errors
  go to zero by construction.
- Rune use: generate the JSON Schema for the catalog **from the Rust component definitions** (one source of truth, `rune
  schema` command), publish it for provider structured-output modes, and run the same schema as the runtime validator. Use
  `oneOf` discriminated by `component`, props `additionalProperties: false`, enums for every theme token (spacing, color
  role, spring name). Keep the schema small: structured-output modes cap schema size/depth and large `oneOf` unions slow
  compile (estimate: keep under ~50 component variants, depth under ~5).
- Cost/risk: low. Constrained decoding fixes syntax and vocabulary, not layout quality or semantics; still needs the lint loop (§6).
- Verdict: **adopt now.**

### 3. Semantic tree as the agent's eyes, with ids shared with screenshots (set-of-marks)  → **adopt now**
- Sources: Set-of-Mark prompting (Yang et al., 2023) **[recall]**; UGround ([arXiv 2410.05243](https://arxiv.org/html/2410.05243v3)): vision-only grounding beats prior models by up to 20% absolute, i.e. pure-pixel agents are viable **[read snippet]**; [ScreenSpot-Pro](https://www.alphaxiv.org/abs/2504.07981) shows high-res professional UIs are where pixel grounding is still weak **[read snippet]**; a 2026 process-centric benchmark evaluates Screenshot vs Screenshot+a11y-tree vs SoM inputs ([ACL Findings 2026](https://aclanthology.org/2026.findings-acl.750.pdf)). OSWorld-Verified has passed the 72.36% human baseline; aggregator sites list leaders at ~85% ([benchlm](https://benchlm.ai/benchmarks/osworld-verified), unverified numbers, treat as indicative only).
- What it does: the two channels are redundant. Trees give exact, cheap, scriptable targets but miss custom-drawn UI (GPU UIs
  such as GPUI/Flutter canvases have empty trees, which is the failure that makes agents fall back to pixels). Screenshots
  see everything but ground imprecisely and cost many tokens per frame. Marks (numbered boxes drawn on the screenshot, same
  numbers in the text tree) combine them.
- Rune use: because the a11y tree *is* the render tree (tenet 3), Rune can guarantee **100% of visible interactive nodes are in
  the tree with rect, role, label, state, source `file:line`**, and render a **marked screenshot** (`mk snap --marks`) where each
  mark id = node id. Agents say `tap n17`, not coordinates. Inject input through the inspector, not synthetic OS events, so a
  tap on a node is exact and timestamped.
- Cost/risk: low; mark overlay is a debug-lane draw pass. The real risk is label quality: models can only use what authors
  write, so make `label` required for every interactive control (compile or lint error with fix).
- Verdict: **adopt now.**

### 4. Token-efficient tree text format (compact, stable ids, diffs)  → **adopt now, measure**
- Sources: browser-agent practice (Playwright MCP snapshots, browser-use, Stagehand) all converged on "pruned a11y tree as
  indented text with ref ids", and [the GUI-agents paper list](https://github.com/OSU-NLP-Group/GUI-Agents-Paper-List)
  tracks work on linearizing a11y trees into compact forms because raw trees are costly and noisy **[read snippet]**.
- What it does: raw DOM/AX dumps run to tens of thousands of tokens per page; pruned role+name+ref outlines are roughly 5-20x
  smaller (estimate from experience, I did not benchmark here).
- Rune use, a concrete format:
  ```
  window "Inbox" 390x844
    list key=threads  (scroll y=0/2310)
      row n17 key=thread:42 "Ana: lunch?" [selected] 358x64 @16,120  <ThreadRow src=inbox.rs:88>
        text n18 "Ana" ; text n19 "lunch?" ; button n20 "Archive"
  ```
  Rules: only semantic and visible nodes; collapse pure layout wrappers; ids derived from keys (stable across frames, so
  `tap n17` survives re-render); layout rects optional (`--rects`); **diff mode** returns only changed nodes since frame id
  N (the channel already carries tree diffs; surface the same diff as text). Add a token counter to every inspector reply so
  size is a tracked metric.
- Cost/risk: trivial to build; the work is deciding what is "semantic" and keeping it stable. Add a golden test per scene on
  the text tree: it doubles as a cheap, deterministic UI regression test that is far less flaky than pixels.
- Verdict: **adopt now.**

### 5. Self-verifying loop: render, lint, critique, patch  → **adopt now (this is the product)**
- Sources: Google's Generative UI paper uses **post-processors for error reporting and page analysis** and notes JS/CSS/HTML
  errors still occur occasionally ([arXiv 2604.09577](https://arxiv.org/abs/2604.09577)), plus prompt ablation: full
  prompt ELO 1553 vs minimal 1496 vs no-"philosophy" 1451, so instructions matter more than examples-free brevity **[read]**.
  DesignBench ([arXiv 2506.06251](https://arxiv.org/abs/2506.06251)) separates generation, edit and repair tasks and finds
  framework-specific limitations and that generation tasks fail mostly on spatial/structural errors ("wrong element size",
  "element missing") **[read abstract+snippet]**. Design2Code (484 real pages, [Si et al. 2024](https://salt-nlp.github.io/Design2Code/)): omission of elements and
  layout distortion are the dominant visual failures; text and color are easy **[recall]**.
- What it does: failures are *spatial and structural*, which is exactly what exact layout lint can catch without a vision
  model. A visual critique by a model is the second pass, not the first.
- Rune use: the PLAN §6 tools already are this. Make it one command an agent runs after every edit: `rune check <scene>` =
  compile + render scene + the internal layout linter (overflow, overlap, tap target < 44, off-grid, contrast) + the internal motion linter on a scripted
  interaction + tree diff vs golden, and print **findings as fixes keyed to source locations** ("`Row` at inbox.rs:88 is 312
  pt wide in a 290 pt parent; set `.fill()` or wrap in `Scroll`"). Return a marked screenshot only when lint passes or the
  agent asks. One structured result, small text, hot-reload speed (<1 s). This is what turns first-try into
  second-try-within-seconds.
- Cost/risk: moderate engineering, but all pieces are already in the plan. Risk: findings must be precise; a noisy linter
  trains agents to ignore it.
- Verdict: **adopt now.**

### 6. Design-system-constrained generation (tokens, not values)  → **adopt now**
- Sources: A2UI catalog `instructions` + `theme` ([spec](https://a2ui.org/specification/v0.9.1-a2ui/)); json-render's
  `defineCatalog` with Zod props and `catalog.prompt()` that auto-generates the system prompt
  ([repo](https://github.com/vercel-labs/json-render), 18.8k stars, Apache-2.0; has an `@json-render/mcp` package and Ink/RN/Remotion/PDF renderers) **[read]**.
  Google's paper adds "consistent styling" as an option ([arXiv 2604.09577 §2.1](https://arxiv.org/html/2604.09577)) **[read]**.
- What it does: the model picks `spacing: "md"`, `color: "text.secondary"`, `motion: "snappy"` instead of `13px`, `#8E8E93`,
  `0.35s ease`. No off-palette color, off-grid spacing or unspecified curve is expressible. This also makes output
  automatically consistent with the app's theme (tenet 5).
- Rune use: the catalog's prop types are **enums of theme tokens only** plus a few numeric ranges with units. No raw hex, no
  raw durations, no free-form style objects in the generative lane (they stay available in the trusted app lane). Generate the
  prompt from the catalog (`rune prompt` emits catalog + rules + 3 scenes as few-shot) so docs and validator can't drift.
- Cost/risk: the catalog needs to be expressive enough that models don't hit walls; plan an escape hatch component (`Canvas`
  with a restricted vector DSL) rather than loosening the main catalog.
- Verdict: **adopt now.**

### 7. Generative UI quality and latency envelope (Google)  → **calibrate expectations; later**
- Source: [Google Research blog](https://research.google/blog/generative-ui-a-rich-custom-visual-interactive-user-experience-for-any-prompt/) (Nov 2025), [paper](https://arxiv.org/abs/2604.09577). **[read]**
- Numbers: LMArena prompts, human pairwise preference: Generative UI beats markdown 82.8%, beats top search result 90%, loses to
  human-expert sites (35-43% win depending on revision; paper versions disagree: v1 says 35.3%, the PDF says 43%). ELO ~1736 vs
  expert 1800 vs markdown 1438. Capability is *emergent*: newest models show far fewer generation errors. **Generation takes
  "a minute or two"; streaming halves the time-to-interaction.** Output is a single full HTML page rendered as-is, i.e. maximal
  freedom, no catalog.
- Rune use: two lessons. (a) Latency is the problem, so a protocol that renders progressively on partial output (A2UI's
  flat list + placeholder rule) is mandatory, and the 1-2 minute whole-page approach is not something to copy. (b) Quality
  is model-driven and improving, so a catalog design that is slightly too restrictive today is a bet that models will not need
  raw HTML. Plan the catalog to grow.
- Verdict: **later** for Rune-as-chat-surface; the research mostly validates #1, #2, #6.

### 8. Sandboxed-code lane: MCP Apps and the OpenAI Apps SDK  → **skip for rendering; adopt as the delivery pipe**
- Sources: [SEP-1865 MCP Apps, Stable 2026-01-26](https://github.com/modelcontextprotocol/ext-apps/blob/main/specification/2026-01-26/apps.mdx); [MCP Apps overview](https://modelcontextprotocol.io/extensions/apps/overview); [Microsoft Cowork adoption](https://learn.microsoft.com/en-us/microsoft-365/copilot/cowork/mcp-apps-support). **[read]** OpenAI Apps SDK (Oct 2025) is the same pattern (tool declares a UI template, sandboxed iframe, host bridge) **[recall]**.
- What it is: servers declare `ui://` HTML resources (`text/html;profile=mcp-app`) on tools via `_meta.ui.resourceUri`; hosts
  render them in a sandboxed cross-origin iframe, CSP built from declared domains (default: no network), JSON-RPC over
  `postMessage`, template/data separation, host-mediated tool calls with user consent. Adopted by Claude, ChatGPT, VS Code, Cowork.
- Why it matters for Rune: it is the **de facto distribution channel** for agent UI: any Rune-hosting product (OpenCode
  Desktop) that wants to show third-party agent UI will be asked for it. But HTML-in-iframe is the opposite of native feel:
  no shared springs, text, a11y tree or glass.
- Rune use: (a) **steal the security model verbatim for any untrusted code lane:** predeclared templates, no ambient network,
  declared capabilities, all host calls as auditable messages with consent. (b) Offer a **Rune-native MCP Apps profile**: a new
  `mimeType` for a Rune surface (A2UI-shaped JSON) behind the same `ui://` + `_meta.ui` linkage. The SEP reserves other content
  types for future extensions. Tools stay unchanged; hosts that support the Rune profile render natively, others fall back to text.
- Cost/risk: spec is Stable so safe to target; a native profile needs host adoption, so spike with OpenCode Desktop first.
- Verdict: **spike** (native profile), **skip** (iframe rendering inside Rune).

### 9. Server-driven UI in production (Airbnb Ghost Platform, Lyft, Shopify)  → **steal the lessons**
- Sources: Airbnb "Ghost Platform" series (2021), Lyft Bikes and Scooters SDUI (2022-23), Shopify Mobile SDUI/"Remote UI" **[all recall, older than 2023, not re-fetched]**.
- Lessons that recur: (1) a **section/component registry versioned with the client**, because old clients must ignore or degrade
  unknown components (negotiate capabilities up front, like A2UI's catalogId + capabilities exchange); (2) **layout and
  data separated** so data refreshes don't resend structure; (3) **actions are a closed set** (navigate, setState, call
  tool), never scripts; (4) the failure mode is the **escape-hatch creep** where "just one more prop" turns the schema
  into a bad programming language (json-render's `$cond/$state/$computed` already shows the slope); (5) debuggability needs
  a spec-inspector (json-render ships devtools: spec tree, state editor, action log, stream log).
- Rune use: version the catalog with semver per component, advertise `catalogId`+version on connect, define degrade rules
  (`unknown → Placeholder with label`). Keep expressions to a deliberately tiny set: bind, cond, template, a registered
  function list; no arithmetic beyond the catalog's `format*` functions. Ship a `rune surface` inspector that shows incoming
  messages next to the resulting tree.
- Verdict: **adopt now** (as design constraints).

### 10. Rust codegen failure modes → design the API against them  → **adopt now**
- Sources: [Rust-SWE-bench, ICSE 2026](https://arxiv.org/abs/2602.22764): 500 tasks, 34 repos; best ReAct agents resolve up to 21.2%; the two named
  bottlenecks are **repo-wide structure comprehension and compliance with Rust's strict type and trait semantics**; their RustForger
  (issue reproduction + metaprogramming tracing) reaches 28.6% **[read]**. [Microsoft Research on LLMs fixing Rust compile errors](https://www.microsoft.com/en-us/research/wp-content/uploads/2024/08/paper.pdf) (2024) and
  [Fortytwo's Strand-Rust-Coder report](https://huggingface.co/blog/Fortytwo-Network/strand-rust-coder-tech-report) both report borrow-checker violations as
  the dominant base-model failure **[read snippets]**. Caveat: these are issue-resolution benchmarks on 2025-vintage models; no
  published study I found measures *Rust UI framework* first-try compile rates. Treat any number about Rune specifically as
  unmeasured until Rune runs its own eval (see §Recommendations: "Rune-Bench").
- What it implies: errors come from (a) lifetimes and borrows, (b) deep generics/trait bounds, (c) misremembered APIs
  (training data predates Rune; GPUI/Iced/Xilem APIs shift and models hallucinate them).
- Rune use: view code should require **no lifetimes, no generic bounds, no borrows crossing closures** in the common path:
  `Copy` ids and handles, owned values, closures take `&mut Model` passed in, no `impl Trait` returns that need naming, one
  concrete `El` type (the PLAN's type-erased debug mode helps). Fewer distinct concepts than SwiftUI. Ship `llms.txt` plus
  **an examples directory that compiles in CI** and prefer many small canonical examples over prose (models copy examples).
  Make compiler errors actionable via custom `#[diagnostic::on_unimplemented]` and `compile_error!` messages with the fix.
- Verdict: **adopt now.**

### 11. Two lanes: trusted Rust app code vs generated surfaces  → **adopt now (architecture)**
- Argument: a model can write Rust app code *at design time* (agent in the loop with a compiler) or emit a **surface** at
  runtime (no compiler). The same Rune renderer must serve both. They differ in trust, latency and expressiveness:

  | Lane | Who writes | Trust | Latency to pixels | Expressiveness |
  |---|---|---|---|---|
  | App code (Rust, hot reload) | coding agent + human | full | ~300 ms hot patch | everything |
  | Surface (A2UI-shaped JSON over the channel) | model at runtime, or server | **untrusted**: catalog only, validated, capability-limited | first partial in one token chunk | catalog + data bindings + closed actions |
  | Plugin (future, WASM) | third party | sandboxed, capabilities declared | compile-time | restricted |

- Rune use: the surface lane is just another *producer* feeding the same render-thread channel the app side feeds. Each
  surface gets its own **sub-tree root with a resource budget** (node count, text bytes, image count, spring count, depth, layout
  cost) enforced before commit; exceeding it truncates with a visible placeholder, never a frame drop (§6b "no dropped frames by budget" applied at runtime).
- Verdict: **adopt now** (shapes the channel protocol: tree producers are pluggable, ids are namespaced per producer).

### 12. Runtime safety for model-generated surfaces  → **adopt now (checklist), spike (fuzzing)**
- Threats: prompt-injected content rendering deceptive UI (fake system dialog, fake "Allow" button), resource exhaustion
  (10^6 nodes), exfiltration via image URLs or `openUrl`, action spoofing, layout-time bombs (deep nesting, huge text).
- Rune use (all enforceable because Rune owns the pixels):
  - **Provenance chrome:** every surface lives inside a visually distinct container the model cannot style away (inset glass
    border, optional "generated" badge); surfaces cannot draw outside their bounds (soft mask, the same mechanism as no-hard-clip) or over app chrome.
  - **Closed action set** with host-side policy: `setState` on the surface's own model, `emit(action)` back to the producer,
    `openUrl` only via allowlist and user gesture. Anything that touches the app or network goes through a **capability grant**
    (same shape as MCP Apps `permissions`/consent).
  - **Resource budget + deadline** per surface, enforced at validate time and each frame; image loads through a host proxy
    with domain allowlist (MCP Apps' `connectDomains/resourceDomains` model).
  - **Validate then commit atomically:** patches that fail schema or budget are rejected whole and reported back to the model as
    a structured error (the model fixes it next turn). No half-applied surfaces.
  - **Input from surfaces is data, never code:** no expression language beyond the tiny set in §9.
  - Fuzz the validator + layout with generated surfaces (property tests; the PLAN's scene fuzzing applies).
- Verdict: adopt the checklist; **spike** a validator fuzzer early.

### 13. Native feel for model output: motion and layout come from the catalog, not the model  → **adopt now**
- Insight: models can't be trusted to tune springs, and shouldn't. The catalog gives each component built-in
  enter/exit/layout behavior from theme tokens (bounce 0 default, duration-based), identity from `id`, so streaming in a new
  card, reordering a list, or the model rewriting the whole surface all animate correctly with no motion authored by the model.
  Progressive rendering becomes a feature: A2UI-style streamed `updateComponents` inserts nodes that **fade/scale in with presence
  tracks while text streams**, and layout animates as siblings arrive. The internal motion linter's rules (pop, jump, reflow) run on
  surfaces too, so you can **prove** a streaming surface never jumps.
- Specific risk: streaming makes *layout churn* (content height grows token by token). Policy: reserve space (skeleton
  placeholders by component default size), apply layout edge tracks with the standard spring, never animate every token; text
  grows with a soft trailing mask rather than reflowing the box each token (precedent: the private iOS client's streaming text).
- Verdict: **adopt now**, as a test scene: "stream a 40-component surface at 30 tokens/s, the internal motion linter must be clean."

### 14. Rune-Bench: our own eval of first-try correctness  → **adopt now (small)**
- Why: no public benchmark measures LLM first-try correctness for a new Rust UI framework or catalog. Closest: DesignBench
  (React/Vue/Angular generation/edit/repair), Design2Code, WebGen-Bench (end-to-end web apps, pass rate by UI tests; [recall]), Sketch2Code.
  Their taxonomy (generation / edit / repair; structural vs visual errors) is what to copy.
- Rune use: 30-50 tasks as scenes (spec + fixture + reference tree/golden): *generate from prompt*, *edit* ("make the row
  tappable"), *repair* (given lint failure). Metrics: compiles first try, lint-clean first try, tree-match vs reference, turns to green, tokens spent. Run per model on
  every API change. **This turns "API agents can't misuse" into a number**, bisectable like the perf counts.
- Verdict: **adopt now** (week 1 of M1-ish); cheap and pays back on every API decision.

## Steal / avoid

**Steal**
- A2UI's flat element list + ids, separate data model, four-message envelope, catalogId negotiation, catalog `instructions`.
- json-render's: catalog → auto-generated prompt; SpecStream (streamed JSON patches compiled into a partial spec); devtools panel (spec tree, state, action log, stream log).
- MCP Apps's security model: predeclared templates, restrictive default, declared capabilities, auditable messages, user consent for UI-initiated calls; `ui://` linkage as a delivery vehicle.
- Browser-agent snapshot practice: indented role/name/ref text, ref ids stable within a snapshot.
- Set-of-marks overlay for pixel+tree grounding.
- Google's "post-processors for common issues": auto-repair passes (e.g. clamp, wrap in Scroll, add missing key) before showing an error to the model.
- DesignBench's generate/edit/repair task split for evals.

**Avoid**
- Whole-page generation as raw HTML/code at runtime (1-2 minute latency, no safety, no catalog, JS errors) as the default.
- A general expression language in the schema (the slope from `$cond` to a bad scripting language).
- Raw style values in the generative lane (hex, px, durations).
- Coordinate-based agent actions as the primary interface; always prefer node ids.
- Making A2UI (still 0.9 → 1.0 candidate, renamed once already) the internal format.
- Screenshot-only verification as the first gate: slow, token-heavy, imprecise; use exact lint first.
- Relying on prompt text to teach the API; models' training data will not contain Rune (unlike SwiftUI/React). Compile-checked examples beat docs.

## Recommendation for Rune

**View API (for the Rust app lane), so models are right the first time**
1. One concrete tree type (`El`), builders only, no macro DSL, no lifetimes or generic bounds visible in signatures. Props via a
   small fixed set of chainable modifiers whose names mirror SwiftUI where semantics match (models know SwiftUI deeply; reuse the
   vocabulary: `Stack`/`HStack`/`VStack`, `.padding`, `.frame`, `.animation`), because **vocabulary alignment with training data is the cheapest first-try-correctness lever**.
2. Tokens, never numbers, in the public API for color, spacing, type, radius, motion (raw numbers only through an explicit `raw_*` escape that lint flags).
3. Explicit `.key()` required in dynamic lists; `label` required for interactive nodes; the compile error text says exactly what to write.
4. Every component registers a **catalog entry** (name, props schema from the Rust type, doc string, 2 scenes). The catalog JSON
   Schema, the `llms.txt` section, the inspector's queryable names, and the generative lane's validator are generated from that single registry.
5. Examples dir compiled and run in CI; `rune check` as the single feedback command (§5 above).

**Wire protocol (command channel, used by app side, hot reload, tests, agents, generated surfaces)**
- Messages (a superset of A2UI's four): `surface.create{id, catalog, version, budget}`, `surface.patch{id, ops}` (ops: upsert
  element by id, remove, reorder children, set data path), `surface.delete`, plus lower-level render commands (targets, springs, text runs) that only trusted producers may send.
- Elements are a flat table `{id, kind, props, children[]}`; ids namespaced `producer/surface/local`; props reference data
  paths or theme tokens; a validate step produces a typed `Rejection{path, rule, fix}` that goes back to the producer (and into the model's context).
- Events back: `action{surface, element, name, payload}`, `settled{frame}`, `data{surface, model}` (A2UI `sendDataModel`).
- Same stream recorded for replay (already planned), same stream carried over the inspector socket, same stream a `--agent` text view renders as the compact tree. One protocol, four consumers.
- Negotiate on connect: catalog id + semver, budget limits, supported motion tokens, max protocol version.

**Rendering model-generated UI at runtime with native feel**
- Surface lane = declarative catalog only, validated + budgeted + provenance-marked (§12); trusted components provide springs,
  text, a11y, glass. A2UI/MCP Apps adapters sit at the edge: A2UI JSON → surface patches; a Rune MCP Apps profile for hosts that opt in.
- Untrusted code (later): WASM component with host-granted capabilities, emits surface patches, never touches the render
  thread; MCP Apps' consent/CSP model as the template. Not before M-late.

**Rejected alternatives**
- *Embed a WebView for generated UI (MCP Apps iframe):* gives universal compatibility, loses everything Rune is for. Allow only as a clearly separate "web card" component if a host demands it.
- *Let the model write Rust at runtime and compile:* seconds-to-minutes compile, borrow-checker failure rates (Rust-SWE-bench: up to 21% issue resolution), no safe sandbox. Keep for design time only.
- *Adopt A2UI wholesale as the one format:* its component model has no motion, no layout animation, no a11y labels as required fields, and is mid-churn. Adapter, not foundation.
- *Screenshot-only agent operation:* strong models now handle it (OSWorld-Verified past the human baseline per aggregators), but it's costlier, noisier and misses state that the tree gives for free.

## Order of work (suggested)
1. Define component registry → JSON Schema + `llms.txt` + prompt generator (items 2, 6, 10).
2. Compact tree text + marks screenshot + stable ids in inspector (3, 4).
3. `rune check` single-command verify loop with fix-oriented findings (5).
4. Rune-Bench v0 with 30 scenes (14).
5. Surface lane: patch protocol, validator, budgets, provenance chrome, fuzzer; A2UI adapter (1, 11, 12).
6. Streaming-surface motion scene under the internal motion linter (13).
7. Spike: Rune MCP Apps profile in OpenCode Desktop (8). Later: WASM plugin lane.

## Open questions
- What is first-try compile + lint-clean rate for Rune's API per model today? Unknown until Rune-Bench exists; set the target
  (suggest ≥80% generate, ≥90% edit within 2 turns) and track.
- How small can the catalog stay (estimate 30-50 components) before models hit walls, versus how big before constrained-decoding schema compile hurts per provider?
- Can the surface lane share the exact `El` diff path (zero extra work on the render thread), or does namespacing/budgeting need its own tree? Probably shared; prove with the counting allocator.
- Do hosts (OpenCode Desktop, Claude, ChatGPT) have any appetite for a native-surface MCP Apps profile? Needs a conversation with OpenCode before building.
- Text-tree vs marks: which gives better agent success per token on Rune scenes? Needs an A/B over Rune-Bench with two agent configs.
- Evidence gaps in this note: Set-of-Mark, Design2Code specifics, WebGen-Bench, Sketch2Code, Airbnb/Lyft/Shopify SDUI details, and the OpenAI Apps SDK are **[recall]**, not re-read; OSWorld-Verified leaderboard figures come from aggregator sites and could be wrong. Re-verify before quoting externally.
