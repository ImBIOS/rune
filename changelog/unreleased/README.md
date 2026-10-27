One file per change, `<YYYY-MM-DD>-<slug>.md`, holding one top-level Markdown bullet in the style of the `CHANGELOG.md` entries.
`scripts/changelog-fold` prepends the fragments under `## dev` (newest first) and deletes them; `--check` shows what it would do.
