# Syntax highlighting in Markdown code fences

How rune-kit colours fenced code, and why it uses hand-written scanners instead of a grammar
engine.

## What it has to do

- Colour the languages chat answers use most: TypeScript/TSX/JavaScript, JSON, Rust, Swift,
  CSS, HTML, shell, SQL and YAML. Anything else stays plain.
- Treat the input as untrusted: model output can be huge or adversarial, so time must be linear
  in the source with no backtracking, and work per block must be capped.
- Stream without layout shift: a fence grows a line at a time, and colouring must not move a
  glyph.
- Build everywhere Rune builds, including wasm and iOS, without adding much to binaries.

## Options

**syntect** (Sublime Text grammars). The most accurate of the three for many languages, but
it ships a serialized grammar set (hundreds of KB to a few MB, depending on the languages kept),
loads it lazily at a noticeable startup cost, and its grammars rely on regexes with lookaround,
run by `onig` (C) or `fancy-regex`. Both backtrack, so a crafted line can take super-linear
time. Highlighting is per line with carried state, which fits streaming, but the cost and the
backtracking risk don't fit untrusted chat text.

**tree-sitter**. Real incremental parsing and the best structure, but each language is a
generated C parser (the TypeScript and TSX grammars alone are large), the runtime is C, and
highlighting needs a query file per language. Every target, wasm and iOS included, then needs a
C toolchain, and the size cost is several hundred KB per language. It is the right tool for an
editor, more than a chat transcript needs.

**Hand-written scanners** (chosen). One small byte scanner per language family (C-like, shell,
markup, CSS, SQL, YAML), in the same spirit as the native mermaid renderer. Each only moves
forward; lookaheads are bounded (a regex literal at most 512 bytes) or skip bytes the scanner
would otherwise visit; embedded code (template `${}`, JSX `{}`, `<script>`, `$()`) nests at most
three deep. Pure Rust, no dependencies, no data files, wasm-friendly. The cost is semantic
precision: it recognises tokens, not grammar, so an unusual construct can colour slightly
wrong. For reading an answer that is an acceptable trade, and the scanners err toward leaving
text plain.

## Decisions

- **Colour only.** Spans set a brush and nothing else: no weight, italic or size, so shaping
  sees the same runs and glyph positions are identical with and without colour. A unit test
  lays out the same code plain, coloured and part-coloured and compares every glyph quad.
- **Streaming.** An unclosed fence colours its complete lines only; the line still arriving
  draws plain and takes its colour when its newline arrives.
- **Caps.** The first 32 KiB of a block (cut at a line end) and at most 8192 coloured runs;
  the rest draws plain. Results are cached per (language, source) for the last 32 blocks on
  the thread, the same bound as the parse cache.
- **Colours.** Eight token kinds from `MarkdownStyle`. Defaults use fixed familiar hues (violet
  keywords, green strings, orange numbers, teal types, blue functions, rose properties) at one
  lightness and chroma derived from the theme's text colour, and the theme's muted and faint
  text for punctuation and comments. Theme status colours were tried as the hue source, but
  they vary too much between themes (one theme's "info" is magenta) to keep kinds apart.
- **No new feature flag.** The scanners are about a thousand lines of plain Rust with no
  dependencies, so they live under `markdown`.
- **`Block` keeps its shape.** Each code block's language travels beside the parsed blocks
  rather than in a new `Block` variant, so apps that match on `Block::Code` keep working.
