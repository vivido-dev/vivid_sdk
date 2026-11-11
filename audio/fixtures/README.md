# Synthetic audio fixtures

These JSON files contain hexadecimal canonical initialization and access units
from generated 440 Hz tones at 48 kHz stereo. They contain no recorded or user
media. The PCM output sample count comes from a separate FFmpeg CLI decode of
the original container. Encoder bytes may differ on regeneration; tests use the
checked-in bytes. Opus, AAC and FLAC were generated with FFmpeg 9.0.1, and the
final native Vorbis fixture with the same installation.

Run `python3 generate.py` with FFmpeg/ffprobe and the libopus encoder available,
or pass codec names to regenerate selected fixtures. Native experimental Vorbis
encoding uses a full second because the encoder emits wrapped final packet
durations and excessive padding for some short streams; the generator refuses
wrapped durations. The other tones last 120 ms. No tools or encoders are needed
when running the Rust tests.

Trim comes from packet skip/discard metadata, except Opus leading skip, which
is intentionally left to the decoder's OpusHead handling. The packetization
names follow Vivid 1.5, including raw AAC access units and raw FLAC STREAMINFO.
