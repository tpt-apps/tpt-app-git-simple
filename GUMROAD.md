# Gumroad listing

Everything needed to create/update the Gumroad product once this app is
built. Source copy is `apps.md` §T2-9 in the `tpt-electrician-nz-2` repo — if
that changes, update here too (and vice versa).

## Step-by-step: publish this listing

1. Build the Pro edition (`./build.ps1 -Pro`, once the build pipeline exists
   — see `todo.md` and mirror `tpt-app-fea-lite`/`tpt-app-cutlist-optimizer`'s
   pattern) — produces the packaged exe/zip in `release/`.
2. Log in to Gumroad → **New product** → **Digital product**.
3. Set the product name, price, and upload the packaged file (all below).
4. Paste the short description, bullets, and full description (below) into
   their respective fields.
5. Set category/tags (below) and add a cover image (see note below).
6. Publish, then copy the resulting product URL (`https://gum.co/...`).
7. Paste that URL into the `git-simple` entry's `gumroadUrl` field in
   `tpt-electrician-nz-2\src\lib\tpt-apps-registry.ts`, and set `price: '$15'`
   on the same entry.
8. Flip that entry's `status` to `'live'` once both the free demo and this
   Gumroad listing exist.
9. Commit and push the registry repo.

## File to upload

Not built yet. Once packaged, this will be the Pro exe/zip produced by the
app's `build.ps1 -Pro` (or equivalent) — see `todo.md`.

## Product name

```
TPT Git Simple — Git for People Who Aren't Developers
```

## Price

**$15 USD, one-time purchase** (no subscription).

## Short description / summary field

```
Commit, save, and undo changes to a repo without learning git — built for designers, writers, and PMs, not professional developers.
```

## Bullets / feature list

```
Plain-English actions
Safe undo
Guided conflict resolution
No subscription
One-time purchase
```

## Full description (long-form field)

```
TPT Git Simple is a radically simplified git client with guardrails — "save
my changes," "get the latest version," "undo my last save" — hiding
branches/rebasing/merge conflicts behind sensible defaults.

Who it's for: designers, PMs, technical writers, and translators who
occasionally need to commit changes to a repo (docs, content, translations,
design assets) without learning git. Fork/Tower/GitKraken are built for and
priced at professional developers who already know git concepts — they
don't solve the "I just need to save my change and not break anything"
problem.

Pro edition (this download):
- Full commit/push/pull (free is view-only browsing)
- Guardrails against destructive actions
- Plain-English conflict resolution guidance
- A "just get me back to safe" undo button

Try the free view-only repo browser first, right in your browser, no
install: tptsolutions.co.nz/tools/git-simple

Requirements: Windows 10 (20H2+) or Windows 11. Uses the WebView2 runtime,
preinstalled on both — nothing extra to download.
```

## Category / tags

- Category: **Software > Developer Tools** (or Gumroad's closest equivalent)
- Tags: `git-client`, `non-developers`, `version-control`, `design-tools`,
  `collaboration`, `windows`

## Cover image / thumbnail

Not created yet. A screenshot of the plain-English action buttons ("Save my
changes", "Get latest") is the most convincing cover once built. No
stock/AI art.

## After publishing

1. Copy the resulting product URL (`https://gum.co/...`).
2. Paste it into the `gumroadUrl` field of the `git-simple` entry in
   `tpt-electrician-nz-2\src\lib\tpt-apps-registry.ts`, and set `price: '$15'`.
3. Set `status` to `'live'`.
4. Commit and push the registry repo.
