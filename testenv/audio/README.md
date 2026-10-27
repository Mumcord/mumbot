# Test audio clips

Self-generated, no third-party material: the clip is CC0 (public domain dedication by the Mumcord authors), carried over from Mumcord's `testenv/audio/`.

| File | Content |
|---|---|
| `speech.wav` | 6 s speech-like clip: 16 vowel-like syllables (harmonic source through three formant filters, 105-150 Hz pitch, syllabic envelope, short pauses). Not intelligible; meant for level, timing and codec checks. About -23 dBFS RMS. Used by the voice live tests (`tests/voice_live.rs`). |

Format: 48 kHz, mono, 16-bit PCM.

Regenerate (deterministic, seeded): `python3 testenv/audio/gen_clips.py` (needs numpy). It also writes `speech_noisy.wav` (the clip with fan noise and keyboard clicks, used by Mumcord's noise suppression checks); Mumbot does not need it, so it is not committed.
