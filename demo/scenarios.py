#!/usr/bin/env python3
"""Writes the `tern shot` scenarios the demo video is cut from (demo/scenarios/*.txt)."""
from pathlib import Path

OUT = Path(__file__).parent / "scenarios"
OUT.mkdir(exist_ok=True)

# The prompt, as the demo's author's zsh shows it; `mantern` and a stand-in `man -w` on PATH.
SETUP = (
    'run "export PATH=/workspace/mt/a:/workspace/mt/b:$PATH; '
    "PS1=$(printf '\\\\001\\\\033[38;5;75m\\\\002~/projects\\\\001\\\\033[0m\\\\002 \\\\001\\\\033[38;5;141m\\\\002❯\\\\001\\\\033[0m\\\\002 '); "
    "printf '\\\\033[2J\\\\033[H'\""
)
LESS_COLORS = (
    "LESS_TERMCAP_md=$(printf '\\\\033[1;38;5;9m') LESS_TERMCAP_us=$(printf '\\\\033[1;38;5;10m') "
    "LESS_TERMCAP_ue=$(printf '\\\\033[0m') LESS_TERMCAP_me=$(printf '\\\\033[0m')"
)
PAGE = "/workspace/mt/tar.1"  # demo/shoot.sh unpacks tar.1.gz here
MANDOC = "/nix/store/d6fjifwjq8sli7k7nk157jhk2r69fmrz-mandoc-1.14.6/bin/mandoc"
LESS = "/nix/store/zi4d0awnc6crz18s177bv9y2yz9al3lq-less-710/bin/less"

# Where the clicks land in a 1280x800 window (CSS px), read from the layout JSON of a probe shot.
SEE_ALSO_HEAD = (120, 594 + 64)
GZIP_CHIP = (198, 571)


def typing(prefix: str, text: str) -> list[str]:
    lines = []
    for i, ch in enumerate(text, 1):
        lines += [f'type "{ch}"', "wait 60", f"shot {prefix}{i:02d}"]
    return lines


def classic() -> str:
    define = f"man() {{ {MANDOC} -T utf8 {PAGE} | {LESS}; }}; export {LESS_COLORS}; printf '\\\\033[2J\\\\033[H'"
    return "\n".join([
        "ready", SETUP, f'run "{define}"', "wait 500", "shot c00",
        'run "man tar"',
        "wait 1800", "shot c01",
        'type " "', "wait 500", "shot c02",
        'type " "', "wait 500", "shot c03",
        'type " "', "wait 500", "shot c04",
    ]) + "\n"


def typing_scenario() -> str:
    """Keystrokes only: `run` is what makes the harness wait for the program, so the result
    comes from main()."""
    return "\n".join(["ready", SETUP, "wait 600", "shot m00", *typing("t", "mantern tar")]) + "\n"


def main() -> str:
    steps = ["ready", SETUP, "wait 600", 'run "mantern tar"', "wait 3500", "wait 1500", "shot top"]
    for i in range(1, 91):
        steps += ["scroll -2", "wait 70", f"shot s{i:02d}"]
    steps += ["scroll -9999", "wait 1500", "shot end"]
    steps += [f"click {SEE_ALSO_HEAD[0]} {SEE_ALSO_HEAD[1]}", "wait 150", "shot see0", "wait 700", "shot see1"]
    steps += [f"click {GZIP_CHIP[0]} {GZIP_CHIP[1]}", "wait 200", "shot go0", "wait 700", "shot go1", "wait 2600", "shot go2"]
    steps += ['type "q"', "wait 900", "shot quit"]
    return "\n".join(steps) + "\n"


(OUT / "classic.txt").write_text(classic())
(OUT / "main.txt").write_text(main())
(OUT / "typing.txt").write_text(typing_scenario())
