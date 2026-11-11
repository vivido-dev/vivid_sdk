# Native video regression fixtures

These tiny synthetic streams were generated for this project, without source
media. Both are H.264 Annex B access units at 25 Hz and 16 × 16 pixels.

```sh
ffmpeg -f lavfi -i color=c=red:s=16x16:r=25 -frames:v 3 \
  -c:v libx264 -preset ultrafast -tune zerolatency \
  -x264-params keyint=25:scenecut=0:bframes=0 -f h264 red.h264
ffmpeg -f lavfi -i testsrc2=s=16x16:r=25 -frames:v 6 \
  -c:v libx264 -preset medium \
  -x264-params keyint=25:scenecut=0:bframes=2 -f h264 reordered.h264
```

The checked-in outputs are fixed; encoders from other releases may produce
different bytes. `ffprobe -show_packets` identified byte boundaries:

| File | Access-unit offsets, including end |
| --- | --- |
| `red.h264` | 0, 652, 661, 670 |
| `reordered.h264` | 0, 891, 903, 915, 927, 945, 957 |

Reordered packet PTS values are 0, 120000, 40000, 80000, 200000, 160000
microseconds. Decode-order DTS starts at −80000 and advances by 40000. Tests
cover exact presentation timestamps, reference decoding while discarding old
pictures, EOS drain, output bounds, and retained pixels after decoder retirement.

The existing inline AV1 sequence/header sample was migrated with Vivido's
decoder test. It now runs on all platforms instead of only Windows.
