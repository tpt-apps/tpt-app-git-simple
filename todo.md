# TPT Git Simple — Build Todo

> Hub listing: `tptsolutions.co.nz/tools/git-simple` (registry slug
> `git-simple`, category `developer`). Spec: `apps.md` §T2-9 in the
> tpt-electrician-nz-2 repo. Base is `tpt-gitcastle`. Free version: view-only
> repo browser, no commits. Paid offline edition ($15 USD, Gumroad): full
> commit/push/pull with guardrails, plain-English conflict guidance, safe
> undo. UI on `tpt-appfront`.
>
> Hub contract: `wasm-bindgen --target web` glue exposes `mount(container)`
> (called by `WasmAppRunner` after `default()` init). Standalone `trunk serve`
> mounts into the `#tpt-appfront-root` marker div instead — never
> `document.body` — so the two hosts never double-mount.

## Phase 0 — Scaffold & pipeline proof
- [x] `tpt-appfront init tpt-app-git-simple --target dom` (path deps on the tpt-appfront checkout)
      (scaffold generated to a temp dir and merged by hand into this pre-existing dir;
      `init` refuses an existing directory)
- [x] Restructure scaffold to hub contract: `#[wasm_bindgen] pub fn mount(el)` + `#[wasm_bindgen(start)]` no-op unless `#tpt-appfront-root` present
- [x] `index.html` carries the `#tpt-appfront-root` marker div
- [x] Build pipeline script mirroring fea-lite/cutlist-optimizer's (`--target web` glue → web repo `public/apps/git-simple/`)
- [x] Hello-world build loads through the standalone dev page (`trunk serve`)
      (verified in-browser: mount, heading + buttons render; 126 KB wasm hello world)
- [x] git init + first commit
- [x] Note: base repo is `tpt-gitcastle` — found at `D:\Programming\2 WIP\TPT GitCastle`.
      It is a TypeScript/Electron app (GitHub Desktop lineage), so "wrap tpt-gitcastle"
      cannot mean reusing its code inside a Rust/WASM crate. Phase 1 instead builds a
      pure-Rust git engine (`engine/`) over an abstract `GitFs` trait: native for host
      tests + the Pro desktop exe, wasm32 for the browser (fed by a JS-side directory
      snapshot). GitCastle remains the UX reference for plain-language git.

## Phase 1 — Core engine (replaces "wrap tpt-gitcastle": see Phase 0 note —
   GitCastle is TS/Electron, so the engine is a pure-Rust git implementation)
- [x] Survey `tpt-gitcastle` for existing git operations to reuse —
      conclusion: nothing reusable in Rust/WASM (Electron + Node git); kept
      as UX reference only
- [x] Adapter crate `engine/`: repo in → status/log/diff out; commit message
      + staged files in → commit out; conflict state in → plain-English
      guidance out — plain structs, no UI deps, host-unit-testable
      (`tpt-git-engine`: refs/HEAD, loose + packed objects (idx v2, ofs/ref
      deltas), log, tree walks, Myers line diff, worktree status; `pro`:
      object writing, staged commits, reflog, backup refs, 3-way merge,
      conflict guidance)
- [x] Safe-undo design: every destructive action has a real recovery path —
      reflog appended on every ref update; discards write a backup commit on
      `refs/tpt-git-simple/backups/<ts>-<n>` first (restore reads the tree
      back); undo-last-save is a soft ref move, redo re-reads the reflog
- [x] Validation against a real test repo: `engine/tests/real_repo.rs`
      builds fixtures with the git CLI — reads match `rev-parse`/`log`/
      `ls-tree`/`cat-file`, packed objects verified per-oid, engine-written
      commits pass `git fsck --strict` and `git log` agrees, merge base
      matches `git merge-base`, conflicts match git's, undo/discard/restore
      round trips (cargo test -p tpt-git-engine --features pro)

## Phase 2 — Free tier
- [ ] View-only repo browser: file tree, commit history, diff viewer — no
      commit/push/pull actions
- [ ] Free-tier upsell copy pointing at full commit/push/pull with guardrails,
      conflict guidance, safe undo
- [ ] Browser/desktop-verified: mount, open a repo, browse history and diffs

## Phase 3 — Paid desktop edition (cargo feature `pro`)
- [ ] `pro` gates: save (commit), get latest (pull), push, with guardrails
      (confirm before anything destructive), plain-English merge-conflict
      guidance, safe undo (reflog-backed)
- [ ] Pro UI: "Save"/"Get latest" plain-language actions, conflict resolution
      wizard, undo button
- [ ] Desktop shell via `tpt-appfront-webview`; Windows exe build + smoke test
      on a real git repo including a deliberate conflict scenario
- [ ] Gumroad product from apps.md listing copy ($15), exe upload, URL into
      registry — manual step, listing copy at apps.md §T2-9

## Phase 4 — Ship & measure
- [ ] Registry entry (`developer` category, `wasm: { entry: '/apps/git-simple/git-simple.js' }`)
      per the "Adding a finished app to the hub" runbook at the bottom of apps.md
- [ ] Verify hub card/detail/sitemap/runner in dev, then prod after deploy
- [ ] WASM size budget check on the shipped build
