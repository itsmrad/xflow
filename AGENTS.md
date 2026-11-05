# Code map
Before exploring unfamiliar code, query the shared AST-only map (no model calls; Markdown/MDX excluded).
```sh
MAP="$(git rev-parse --path-format=absolute --git-common-dir)/../graphify-out/graph.json"
graphify query "HotkeyAction" --budget 800 --graph "$MAP"
graphify explain "Engine" --graph "$MAP"
graphify path "Engine" "Services" --graph "$MAP"
```
Use symbol keywords in `query`; English questions are noisy. Budget 800 sufficed for location discovery; narrow or raise it when truncated.
Copy node labels from query output for `explain` and `path`; disambiguate `explain` with `path::symbol` when prompted.
The map can lag main or your worktree; confirm cited source and inferred edges before relying on them.
SQL is absent; graph paths show relationships, not guaranteed execution paths.
Use these commands; never read `graph.json` directly or commit `graphify-out/`.
If missing, build in the primary checkout using the `graphify-code-map` skill (no model calls).
