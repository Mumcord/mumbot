# Test audio clips

Self-generated, no third-party material: the clip is CC0 (public domain dedication by the Mumcord authors), carried over from Mumcord's `testenv/audio/`.

| File | Content |
|---|---|
| `speech.wav` | 6 s speech-like clip: 16 vowel-like syllables (harmonic source through three formant filters, 105-150 Hz pitch, syllabic envelope, short pauses). Not intelligible; meant for level, timing and codec checks. About -23 dBFS RMS. Used by the voice live tests (`tests/voice_live.rs`). |

| `speech.mp3` | The same clip as a 44.1 kHz stereo MP3 (64 kbit/s, ID3 title "Speech sample", artist "Mumbot"), so the music module's tests cover decoding, mixing to mono and resampling. Used by `src/music/` and `tests/music_live.rs`. |

Format of `speech.wav`: 48 kHz, mono, 16-bit PCM.

Regenerate (deterministic, seeded): `python3 testenv/audio/gen_clips.py` (needs numpy); then the MP3 with `ffmpeg -i speech.wav -ar 44100 -ac 2 -b:a 64k -metadata title="Speech sample" -metadata artist=Mumbot speech.mp3`. It also writes `speech_noisy.wav` (the clip with fan noise and keyboard clicks, used by Mumcord's noise suppression checks); Mumbot does not need it, so it is not committed.
