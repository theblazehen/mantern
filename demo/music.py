#!/usr/bin/env python3
"""Synthesises the demo's music (no samples, so nothing to license): a warm pad, a plucked
arpeggio, a soft kick and hats over Am-F-C-G at 108 BPM.

    uv run --with numpy demo/music.py demo/video/public/music.wav [seconds]
"""
import sys
import wave

import numpy as np

SR = 44100
BPM = 108
seconds = float(sys.argv[2]) if len(sys.argv) > 2 else 17.0
out_path = sys.argv[1]
beat = 60 / BPM
n = int(SR * seconds)
t = np.arange(n) / SR
rng = np.random.default_rng(7)
left, right = np.zeros(n), np.zeros(n)


def hz(midi):
    return 440 * 2 ** ((midi - 69) / 12)


def lowpass(x, cutoff):
    a = np.exp(-2 * np.pi * cutoff / SR)
    y = np.empty_like(x)
    acc = 0.0
    for i, v in enumerate(x):
        acc = (1 - a) * v + a * acc
        y[i] = acc
    return y


def add(sig, start, pan=0.0, gain=1.0):
    i = int(start * SR)
    if i >= n:
        return
    seg = sig[: n - i] * gain
    left[i : i + len(seg)] += seg * (1 - max(pan, 0))
    right[i : i + len(seg)] += seg * (1 + min(pan, 0))


chords = [(57, [57, 60, 64]), (53, [53, 57, 60]), (48, [48, 52, 55]), (55, [55, 59, 62])]
bars = int(seconds / (4 * beat)) + 1

for bar in range(bars):
    root, notes = chords[bar % 4]
    start = bar * 4 * beat
    length = 4 * beat + 0.4
    tt = np.arange(int(length * SR)) / SR
    env = np.minimum(tt / 0.35, 1) * np.exp(-np.maximum(tt - 4 * beat, 0) * 8)
    pad = np.zeros_like(tt)
    for m in notes:
        for detune in (-0.12, 0.0, 0.12):
            f = hz(m + 12) * 2 ** (detune / 12)
            pad += (2 * ((tt * f) % 1) - 1) * 0.12  # saw
    pad = lowpass(pad, 900) * env
    add(pad, start, 0.0, 0.5)

    bass = np.sin(2 * np.pi * hz(root - 12) * tt) * env * 0.5
    add(bass, start, 0.0, 0.8)

    # Arpeggio: eighth notes up and down the chord.
    shape = [0, 1, 2, 1, 0, 1, 2, 1]
    for k, idx in enumerate(shape):
        when = start + k * beat / 2 * 1.0 + (beat / 2 if False else 0)
        m = notes[idx] + 24
        pt = np.arange(int(0.9 * SR)) / SR
        f = hz(m)
        pluck = (np.sin(2 * np.pi * f * pt) + 0.4 * np.sin(4 * np.pi * f * pt) + 0.15 * np.sin(6 * np.pi * f * pt))
        pluck *= np.exp(-pt * 5.5) * np.minimum(pt / 0.004, 1)
        add(pluck, when, pan=0.35 if k % 2 else -0.35, gain=0.16)

    # Drums from the second bar: soft kick on every beat, hat on the off beat.
    if bar >= 1:
        for b in range(4):
            when = start + b * beat
            kt = np.arange(int(0.35 * SR)) / SR
            kick = np.sin(2 * np.pi * (50 + 90 * np.exp(-kt * 30)) * kt) * np.exp(-kt * 9)
            add(kick, when, 0.0, 0.7)
            ht = np.arange(int(0.06 * SR)) / SR
            hat = rng.standard_normal(len(ht)) * np.exp(-ht * 70)
            hat = hat - lowpass(hat, 5000)
            add(hat, when + beat / 2, 0.2, 0.22)

# A short feedback delay for width.
delay = int(beat * 0.75 * SR)
for ch in (left, right):
    wet = np.zeros(n)
    wet[delay:] = ch[:-delay] * 0.28
    wet[2 * delay :] += ch[: -2 * delay] * 0.12
    ch += wet
left, right = left + 0.15 * right[::1], right + 0.15 * left[::1]

fade = np.minimum(t / 0.6, 1) * np.minimum((seconds - t) / 1.6, 1)
stereo = np.stack([left * fade, right * fade], axis=1)
stereo *= 0.7 / np.abs(stereo).max()
data = (stereo * 32767).astype("<i2")
with wave.open(out_path, "wb") as w:
    w.setnchannels(2)
    w.setsampwidth(2)
    w.setframerate(SR)
    w.writeframes(data.tobytes())
print(f"{out_path}: {seconds:.1f}s")
