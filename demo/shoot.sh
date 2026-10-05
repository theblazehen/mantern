#!/usr/bin/env bash
# Runs on the Crabbox worker, from the repo: headless Tern shoots scenario files.
#   demo/shoot.sh WxH scenario.txt [scenario.txt...]
# PNGs and layout JSON land in /workspace/shots/out/<scenario>/.
set -euo pipefail
size=$1; shift
S=/workspace/nix-tern/nix/store
T=/nix/store/iv70wa7lnxpvj185lpj7ygwnihwsd868-tern-0.4.5
MANDOC=/nix/store/d6fjifwjq8sli7k7nk157jhk2r69fmrz-mandoc-1.14.6
LESS=/nix/store/zi4d0awnc6crz18s177bv9y2yz9al3lq-less-710
MESA=/nix/store/z5wmfi7agarwjlqb456f2z2r4vsmjad4-mesa-26.2.4
VK=/nix/store/vjdqsn0bz2llabfqa746zfgk4kmswba5-vulkan-loader-1.4.357.0
export VK_ICD_FILENAMES=$S/${MESA#/nix/store/}/share/vulkan/icd.d/lvp_icd.x86_64.json
export LD_LIBRARY_PATH=$S/${VK#/nix/store/}/lib:$S/${MESA#/nix/store/}/lib
mkdir -p /workspace/mt/a /workspace/mt/b /workspace/shots/out
# Stands in for the system man: `man -w [SECTION] NAME` finds the Alpine pages.
cat > /workspace/mt/b/man <<'FAKE'
#!/bin/sh
if [ "$1" = -w ]; then
  shift
  for last; do :; done
  for f in /workspace/alpine/root/usr/share/man/man*/"$last".*; do echo "$f"; exit 0; done
  exit 1
fi
echo "system man: $@"
FAKE
chmod +x /workspace/mt/b/man
gzip -dc /workspace/alpine/root/usr/share/man/man1/tar.1.gz > /workspace/mt/tar.1
install -m755 target/release/mantern /workspace/mt/a/mantern
for sc in "$@"; do
  nix --extra-experimental-features "nix-command flakes" --store /workspace/nix-tern \
    --option build-users-group "" shell $T $MANDOC $LESS \
    -c tern shot "$sc" --out /workspace/shots/out --size "$size" --theme dark 2>&1 | grep -vE "WARN stencil_gfx"
done
