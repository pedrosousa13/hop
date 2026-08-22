# Bundled typefaces

The two families `assets/tokens.css` names, vendored here so they ship inside
the binary rather than being trusted to exist on the host. `tokens.css`'s own
header states the requirement:

> Both are bundled via GResource rather than trusted to be installed. A
> launcher cannot let its identity element fall back silently to generic
> `monospace` on a fresh install.

Issue #198 is where that bundling was actually built; issue #253 (design
refresh, phase 1) re-pointed it from Iosevka Term + Inter to the Geist pair.

## Which faces, and why exactly these

Derived from the `--hop-text-*` declarations in `assets/tokens.css`, not
chosen — every weight below is one some token actually asks for, and no token
asks for anything else. No italics: nothing in the design spec specifies
italic anywhere.

| File | Family | Weight | Asked for by |
| --- | --- | --- | --- |
| `Geist-Regular.ttf` | Geist | 400 | `--hop-text-subtitle`, `--hop-text-empty` |
| `Geist-Medium.ttf` | Geist | 500 | `--hop-text-title`, `--hop-text-hint-label`, `--hop-text-error` |
| `Geist-SemiBoldPlus.ttf` | Geist | 650 | `--hop-text-section` |
| `GeistMono-Medium.ttf` | Geist Mono | 500 | `--hop-text-input`, `--hop-text-calc-result`, `--hop-text-hint-key`, `--hop-text-timestamp` |

The family names in the files are `Geist` and `Geist Mono` (each file's
typographic family — `fc-query` reports Geist Mono Medium as the two-name
list `Geist Mono,Geist Mono Medium`, first entry wins), which is what
`--hop-font-sans` and `--hop-font-mono` name as the first entry of their
fallback chains. Confirmed with `fc-scan -f '%{family}|%{style}|%{weight}\n'`
against each file.

### The 650 face is not an upstream static

Upstream ships statics at 400–900 in steps of 100 (plus a variable font), so
there is no 650 cut to download. `Geist-SemiBoldPlus.ttf` was produced by
instancing the variable font at `wght=650` with `fontTools.varLib.instancer`,
then setting `OS/2.usWeightClass = 650` and naming the style
"Semibold Plus" (`Geist Semibold Plus` / `Geist-SemiboldPlus`) so it can never
be confused with upstream's own 600 SemiBold static. It is a derivative work
in the OFL's sense; see the licence position below for why that is permitted
and what it requires. `fc-scan` reports it as family `Geist`, style
`Semibold Plus`, weight 190 (fontconfig's scale for CSS 650).

## Provenance

Both families were taken from their upstream project's own GitHub release, at
a pinned tag, and their licence text from that same tag rather than from any
copy travelling with a package.

### Geist / Geist Mono

- Upstream: <https://github.com/vercel/geist-font>
- Version: **v1.7.2** (released 2026)
- Asset: the release zip (`geist.zip`), files taken from
  `Geist/ttf/` and `GeistMono/ttf/`; the 650 face instanced from
  `Geist/variable/Geist[wght].ttf` as described above
- Licence text: `LICENSE-Geist.txt` and `LICENSE-GeistMono.txt`, both copied
  from the zip's `OFL.txt` at v1.7.2 — one shared OFL covers both families,
  duplicated under two names to follow this directory's per-family convention

### Checksums

```
0090e004725f6f64b841715b4167920580f883fcf9b67fc6d744089103fec101  Geist-Medium.ttf
5c8968eafb98a4c4f47033daf29e38e284a6f2a82eb017d171ab040fe7c4b615  Geist-Regular.ttf
d35cc1fe0f75c81ff890ba4f22825f4d6fd1377a457740d29f36f345ca079e33  Geist-SemiBoldPlus.ttf
90b15711dc3779b2e64e8aff5228154dd019a90bce4947549c4a8a8a43f2ac25  GeistMono-Medium.ttf
```

## Licence position

**Both families are licensed under the SIL Open Font License, Version 1.1**,
read out of the upstream release itself — not asserted from memory and not
taken from a third-party mirror.

- The zip's `OFL.txt` opens
  `Copyright 2024 The Geist Project Authors (https://github.com/vercel/geist-font)`
  and states `This Font Software is licensed under the SIL Open Font License,
  Version 1.1.`

The OFL permits exactly what this repository does with them. Its own preamble
says so: the fonts "can be bundled, embedded, redistributed and/or sold with
any software provided that any reserved names are not used by derivative
works." hop redistributes three of its four faces unmodified, under their own
names, with this licence text alongside — which is the OFL's condition (§1:
the licence notice must travel with the Font Software).

The fourth face (`Geist-SemiBoldPlus.ttf`) *is* a Modified Version as the
licence defines the term. That is permitted — §1 grants the right to "use,
study, modify and redistribute" — and §3's Reserved Font Name restriction is
not engaged because the copyright statement declares **no Reserved Font
Name**, so nothing forbids the modified face carrying the family name `Geist`
under a distinct style name ("Semibold Plus"). What §3 *does* still require
of a Modified Version is honoured: it never uses a reserved name (there are
none) and the original licence text travels with it, exactly as with the
unmodified faces.

The OFL's copyleft is confined to the Font Software itself: it does not reach
hop's own sources, and it places no condition on the GPL-3.0-only licence the
workspace ships under. This mirrors the reasoning `deny.toml` already applies
to MPL-2.0.

### Why this is not in `deny.toml`

`deny.toml` is a policy over **crates** — `cargo deny` reads the Cargo
dependency graph and knows nothing about a file in `assets/`. A font is not a
crate and will never appear in that graph, so adding `OFL-1.1` to its allow
list would assert a check that does not run. This file is the record instead,
which is why the provenance above is written out rather than summarised.

## Updating a family

Re-download from the upstream release at a new tag, replace the files, and
update the version, checksums and licence text here in the same commit. If a
new release starts shipping a native 650 static, prefer it over the locally
instanced face and delete the instancing recipe above. Do not take font files
from a distribution package or a font mirror: the point of the pinned
upstream tag is that the licence text above describes the exact bytes that
ship.
