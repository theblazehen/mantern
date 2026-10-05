# mantern

`man` pages as native UI in the [Tern](https://docs.stencil.so/tern/) terminal, over the Tern
Surface Protocol only (no plugin). It takes exactly `man`'s arguments:

```sh
mantern tar
mantern 5 ssh_config
```

Pages are parsed with [libmandoc](https://crates.io/crates/libmandoc-rs) and drawn as a title block,
a synopsis card, option cards with accent flags, code blocks and tables. Long pages open on NAME,
SYNOPSIS and DESCRIPTION with the other sections folded; click a heading to open it.
SEE ALSO entries are chips: click one and that page opens right below. `q` (or ^C / ^D) closes the
page, which stays in the scrollback. `name(1)` references in running text link to man.archlinux.org.

Anything it doesn't draw runs the system `man` unchanged: more than one page, flags such as
`-k`/`-f`/`-w`, output that isn't a terminal, tmux/screen/zellij, a terminal without TSP `flow`
surfaces (Tern 0.4.5 or newer), or `MANTERN=0`.

| Variable | Effect |
| --- | --- |
| `MANTERN=0` | always use the system `man` |
| `MANTERN_FOLD=0` | never fold sections |
| `MANTERN_DEBUG=1` | say why the system `man` was used; log the terminal's reply and input |
| `MANTERN_TSP_RECORD=file` | record the messages sent as JSONL (`tern` `surface-play` format) |

To use it as `man`, alias it: `alias man=mantern`. It finds the system `man` on `PATH` (skipping itself).

## Development

`mise run corpus` parses every page on the machine and reports coverage. `demo/` holds the scripts
that shoot the demo video with a headless Tern and cut it in Remotion (`demo/shoot.sh`,
`demo/scenarios.py`, `demo/video/`, `demo/music.py`).
