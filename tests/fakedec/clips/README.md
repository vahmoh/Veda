<!-- SPDX-FileCopyrightText: © 2026 Vahid Mohammadi -->
<!-- SPDX-License-Identifier: GPL-3.0-only -->

# Fake videos

Videos for `fakedec`, the stand-in decoder (see `../src/clip.rs` for the
format), installed at `/system/tests/videos/fake` for the GUI scripts
(`tests/ui/videos-fake.vts`):

| File | What it tests |
|------|---------------|
| `pattern.mkv` | playback, pausing, seeking, stepping, the end (720p, 30 fps, 20 s) |
| `train.mp4` | a 23.976 fps video (12.5 s) |
| `portrait.mp4` | a portrait video without sound |
| `crash.mkv` | the decoder crashing at 6 s, twice: restarted, then past it |
| `hang.mkv` | the decoder hanging at 5 s: the watchdog |
| `slow.mkv` | a decoder too slow to keep up: skipping pictures |
| `broken.webm` | a file that is not a video |
