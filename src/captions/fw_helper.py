# CINVAR faster-whisper helper: argv = audio_wav out_srt [language]. CPU, int8, no network unless
# CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD=1. The model is a local directory/cache name.
import os, sys
from faster_whisper import WhisperModel

audio, out = sys.argv[1], sys.argv[2]
lang = sys.argv[3] if len(sys.argv) > 3 and sys.argv[3] else None
model = os.environ.get("CINVAR_FASTER_WHISPER_MODEL", "tiny")
root = os.environ.get("CINVAR_FASTER_WHISPER_CACHE") or None
local_only = os.environ.get("CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD") != "1"
m = WhisperModel(model, device="cpu", compute_type="int8", download_root=root, local_files_only=local_only)
segments, info = m.transcribe(audio, language=lang, vad_filter=True)

def ts(t):
    ms = int(round(t * 1000))
    return "%02d:%02d:%02d,%03d" % (ms // 3600000, ms // 60000 % 60, ms // 1000 % 60, ms % 1000)

n = 0
with open(out, "w", encoding="utf-8") as f:
    for s in segments:
        text = s.text.strip()
        if not text:
            continue
        n += 1
        f.write("%d\n%s --> %s\n%s\n\n" % (n, ts(s.start), ts(max(s.end, s.start + 0.05)), text))
