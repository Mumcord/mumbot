#!/usr/bin/env python3
"""Synthesizes the test clips (deterministic; no external audio). Needs numpy + stdlib wave.

    python3 testenv/audio/gen_clips.py   # writes speech.wav and speech_noisy.wav here
"""
import wave
from pathlib import Path

import numpy as np

SR = 48_000
DUR = 6.0
rng = np.random.default_rng(20260101)
t = np.arange(int(SR * DUR)) / SR


def biquad_bandpass(x, f0, q):
    """RBJ constant-0dB-peak band-pass, direct form I."""
    w = 2 * np.pi * f0 / SR
    alpha = np.sin(w) / (2 * q)
    b0, b2 = alpha, -alpha
    a0, a1, a2 = 1 + alpha, -2 * np.cos(w), 1 - alpha
    y = np.zeros_like(x)
    x1 = x2 = y1 = y2 = 0.0
    for i, xi in enumerate(x):
        yi = (b0 * xi + b2 * x2 - a1 * y1 - a2 * y2) / a0
        x2, x1, y2, y1 = x1, xi, y1, yi
        y[i] = yi
    return y


# Syllables: (start s, length s, base pitch Hz, formants F1/F2/F3)
VOWELS = [(700, 1200, 2600), (300, 2300, 3000), (500, 1000, 2500), (350, 800, 2400), (600, 1700, 2600)]
syllables = []
pos = 0.15
for i in range(17):
    length = rng.uniform(0.14, 0.26)
    syllables.append((pos, length, rng.uniform(105, 150), VOWELS[i % len(VOWELS)]))
    pos += length + (rng.uniform(0.25, 0.55) if i % 5 == 4 else rng.uniform(0.02, 0.07))
    if pos > DUR - 0.4:
        break

speech = np.zeros_like(t)
for start, length, f0, formants in syllables:
    n0, n1 = int(start * SR), int((start + length) * SR)
    tt = np.arange(n1 - n0) / SR
    f = f0 * (1 + 0.12 * np.sin(np.pi * tt / length))  # falling/rising contour
    phase = 2 * np.pi * np.cumsum(f) / SR
    src = sum(np.sin(h * phase) / h for h in range(1, 30))  # harmonic-rich glottal source
    voiced = sum(biquad_bandpass(src, fm, 8) * g for fm, g in zip(formants, (1.0, 0.7, 0.35)))
    env = np.sin(np.pi * tt / length) ** 1.5
    speech[n0:n1] += voiced * env
speech += 0.01 * biquad_bandpass(rng.standard_normal(len(t)), 4500, 1.5) * (np.abs(speech) > 0.02)
speech *= 0.5 / np.max(np.abs(speech))  # peak -6 dBFS


def noise_bed():
    # Fan: low-passed noise + blade-pass hum; keyboard: clicks with a short resonant ring.
    fan = biquad_bandpass(rng.standard_normal(len(t)), 220, 0.7) * 0.5
    fan += 0.02 * np.sin(2 * np.pi * 118 * t) + 0.01 * np.sin(2 * np.pi * 236 * t)
    keys = np.zeros_like(t)
    for at in np.sort(rng.uniform(0.1, DUR - 0.1, 22)):
        n0 = int(at * SR)
        n = int(0.025 * SR)
        click = rng.standard_normal(n) * np.exp(-np.arange(n) / (0.002 * SR))
        click = biquad_bandpass(click, rng.uniform(2200, 4200), 3)
        keys[n0 : n0 + n] += click * rng.uniform(0.5, 1.0)
    return fan, keys


fan, keys = noise_bed()
fan *= 0.06 / np.sqrt(np.mean(fan**2))  # about -24 dBFS RMS
keys *= 0.15 / np.max(np.abs(keys))
noisy = speech + fan + keys
noisy *= 0.5 / np.max(np.abs(noisy))


def write(name, x):
    pcm = (np.clip(x, -1, 1) * 32767).astype("<i2")
    with wave.open(str(Path(__file__).with_name(name)), "wb") as w:
        w.setnchannels(1)
        w.setsampwidth(2)
        w.setframerate(SR)
        w.writeframes(pcm.tobytes())


write("speech.wav", speech)
write("speech_noisy.wav", noisy)
rms = lambda x: 20 * np.log10(np.sqrt(np.mean(x**2)))
print(f"speech {rms(speech):.1f} dBFS, noisy {rms(noisy):.1f} dBFS, {len(syllables)} syllables")
