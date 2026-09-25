# TPT Git Simple

A radically simplified git client — "save my changes," "get the latest
version," "undo my last save" — for designers, PMs, writers, and translators
who occasionally need to commit to a repo without learning git.

- **Free in-browser edition**: view-only repo browser (file tree, commit
  history, diffs). Ships as a wasm-bindgen bundle to
  `tptsolutions.co.nz/tools/git-simple`.
- **Pro desktop edition ($15, Gumroad)**: plain-language save (commit), get
  latest (pull), push — with guardrails, plain-English conflict guidance,
  and reflog-backed safe undo. A Windows webview exe.

Built on [TPT AppFront](../tpt-appfront) (DOM backend). Engine is a pure-Rust
git implementation (`engine/`) over an abstract filesystem trait, so the same
code runs natively (host tests, Pro desktop) and on wasm32 (browser).

## Development

```powershell
trunk serve          # standalone dev page (mounts into #tpt-appfront-root)
./build.ps1          # free edition: wasm-bindgen bundle -> dist\web
                     #   (set TPT_WEB_REPO to copy straight into the hub's
                     #    public\apps\git-simple\)
./build.ps1 -Pro     # pro edition: trunk --features pro + desktop exe
cargo test -p tpt-git-engine   # engine unit + real-repo tests
```

Listing copy for the Gumroad product lives in [GUMROAD.md](GUMROAD.md); the
build/ship checklist in [todo.md](todo.md). Spec: `apps.md` §T2-9 in the
`tpt-electrician-nz-2` repo.
