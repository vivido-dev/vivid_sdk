"""Generate synthetic packet fixtures and an independent CLI decode reference."""

import json
from pathlib import Path
import subprocess
import sys
import tempfile


def run(args):
    return subprocess.run(args, check=True, stdout=subprocess.PIPE).stdout


def unhex(text):
    return bytes.fromhex("".join(
        line.split(": ", 1)[1].split("  ", 1)[0].replace(" ", "")
        for line in text.splitlines() if ": " in line
    ))


root = Path(__file__).resolve().parent
for codec, encoder, extension, packetization in [
    ("opus", "libopus", "ogg", "opus-packet-v1"),
    ("vorbis", "vorbis", "ogg", "vorbis-packet-v1"),
    ("flac", "flac", "flac", "flac-frame-v1"),
    ("aac", "aac", "m4a", "aac-raw-au-v1"),
]:
    if len(sys.argv) > 1 and codec not in sys.argv[1:]:
        continue
    with tempfile.TemporaryDirectory() as directory:
        path = Path(directory) / ("tone." + extension)
        # A full second avoids the native Vorbis encoder's short-stream padding bug.
        duration = "1" if codec == "vorbis" else "0.12"
        run(["ffmpeg", "-v", "error", "-f", "lavfi", "-i",
             "sine=frequency=440:sample_rate=48000:duration=" + duration, "-ac", "2",
             "-c:a", encoder, "-strict", "-2", str(path)])
        result = json.loads(run(["ffprobe", "-v", "error", "-select_streams", "a:0",
                                 "-show_packets", "-show_streams", "-show_data",
                                 "-of", "json", str(path)]))
        stream = result["streams"][0]
        packets = []
        for packet in result["packets"]:
            side = packet.get("side_data_list", [{}])[0]
            packets.append({
                "data": unhex(packet["data"]).hex(),
                "pts_us": round(float(packet["pts_time"]) * 1e6),
                "duration_us": round(float(packet["duration_time"]) * 1e6),
                # OpusHead pre-skip belongs to the decoder; never duplicate it.
                "trim_start": 0 if codec == "opus" else side.get("skip_samples", 0),
                "trim_end": side.get("discard_padding", 0),
            })
            if packets[-1]["duration_us"] > 1_000_000:
                raise ValueError("encoder emitted invalid wrapped packet duration")
        reference = run(["ffmpeg", "-v", "error", "-i", str(path), "-f", "f32le", "-"])
        fixture = {
            "codec": codec, "packetization": packetization,
            "sample_rate": 48000, "channels": 2,
            "extradata": unhex(stream.get("extradata", "")).hex(),
            "reference_samples": len(reference) // 4,
            "packets": packets,
        }
        (root / (codec + ".json")).write_text(json.dumps(fixture, indent=2) + "\n")
