# Bundled typefaces

The two families `assets/tokens.css` names, vendored here so they ship inside
the binary rather than being trusted to exist on the host. `tokens.css`'s own
header states the requirement:

> Both are bundled via GResource rather than trusted to be installed. A
> launcher cannot let its identity element fall back silently to generic
> `monospace` on a fresh install.

Issue #198 is where that bundling was actually built.

## Which faces, and why exactly these

Derived from the `--hop-text-*` declarations in `assets/tokens.css` (lines
40–49), not chosen — every weight below is one some token actually asks for,
and no token asks for anything else. No italics: nothing in the design spec
specifies italic anywhere.

| File | Family | Weight | Asked for by |
| --- | --- | --- | --- |
| `Inter-Regular.ttf` | Inter | 400 | `--hop-text-subtitle`, `--hop-text-empty` |
| `Inter-Medium.ttf` | Inter | 500 | `--hop-text-title`, `--hop-text-hint-label`, `--hop-text-error` |
| `Inter-SemiBold.ttf` | Inter | 600 | `--hop-text-section` |
| `IosevkaTerm-Regular.ttf` | Iosevka Term | 400 | `--hop-text-timestamp` |
| `IosevkaTerm-Medium.ttf` | Iosevka Term | 500 | `--hop-text-input`, `--hop-text-calc-result`, `--hop-text-hint-key` |

The family names in the files are `Inter` and `Iosevka Term`, which is what
`--hop-font-sans` and `--hop-font-mono` name as the first entry of their
fallback chains. Confirmed with `fc-query -f '%{family}|%{style}|%{weight}\n'`
against each file.

## Provenance

Both families were taken from their upstream project's own GitHub release, at
a pinned tag, and their licence text from that same tag rather than from any
copy travelling with a package.

### Inter

- Upstream: <https://github.com/rsms/inter>
- Version: **v4.1** (released 2024-11-16)
- Asset: `Inter-4.1.zip`, files taken from `extras/ttf/`
- Licence text: `LICENSE-Inter.txt`, fetched from `LICENSE.txt` at tag `v4.1`

### Iosevka Term

- Upstream: <https://github.com/be5invis/Iosevka>
- Version: **v34.8.0** (released 2026-07-26)
- Asset: `PkgTTF-IosevkaTerm-34.8.0.zip` (the hinted TTF package)
- Licence text: `LICENSE-IosevkaTerm.md`, fetched from `LICENSE.md` at tag
  `v34.8.0`

### Checksums

```
97ad806f526e41546d46365bb3a393145f75b7b1568913db74549ad8b8dba872  Inter-Medium.ttf
40d692fce188e4471e2b3cba937be967878f631ad3ebbbdcd587687c7ebe0c82  Inter-Regular.ttf
78a843fade9d4612a5567302fb595b56976eb5fcebf4fea5a5912d638bafcde3  Inter-SemiBold.ttf
b68c97df4d1e832e5cefa5ac5d131174670117ae8916d62d870df08b975c225b  IosevkaTerm-Medium.ttf
5f560f828f39cd696a15734a4767d61d4bfe6e256d7de3bee3ab5c8b46f1acb3  IosevkaTerm-Regular.ttf
```

## Licence position

**Both families are licensed under the SIL Open Font License, Version 1.1**,
and this was read out of each upstream repository at the tag the files came
from — not asserted from memory and not taken from a third-party mirror.

- Inter: `LICENSE.txt` at `v4.1` opens
  `Copyright (c) 2016 The Inter Project Authors (https://github.com/rsms/inter)`
  and states `This Font Software is licensed under the SIL Open Font License,
  Version 1.1.`
- Iosevka: `LICENSE.md` at `v34.8.0` opens
  `Copyright (c) 2015-2026, Renzhi Li (aka. Belleve Invis, belleve@typeof.net)`
  and states the same.

The OFL permits exactly what this repository does with them. Its own preamble
says so: the fonts "can be bundled, embedded, redistributed and/or sold with
any software provided that any reserved names are not used by derivative
works." hop redistributes both unmodified, under their own names, with this
licence text alongside — which is the OFL's condition (§2: the licence notice
must travel with the Font Software). Nothing here is a derivative work, so the
Reserved Font Name clause is not engaged.

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
update the version, checksums and licence text here in the same commit. Do not
take font files from a distribution package or a font mirror: the point of the
pinned upstream tag is that the licence text above describes the exact bytes
that ship.
