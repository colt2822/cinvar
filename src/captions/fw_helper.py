# CINVAR faster-whisper helper: argv = audio_wav out_srt [language] [initial_prompt] [hotwords].
# CPU, int8, no network unless CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD=1. The model is a local
# directory/cache name. initial_prompt/hotwords carry the custom vocabulary and are absent
# unless the caller asked for one, so a run without --vocab is unchanged.
import os, sys
# PyAV >= 15 removed the metadata_errors kwarg that faster-whisper still passes.
import av as _av
_open_orig = _av.open
def _av_open_compat(*args, **kwargs):
    kwargs.pop("metadata_errors", None)
    return _open_orig(*args, **kwargs)
_av.open = _av_open_compat
from faster_whisper import WhisperModel

audio, out = sys.argv[1], sys.argv[2]
lang = sys.argv[3] if len(sys.argv) > 3 and sys.argv[3] else None
model = os.environ.get("CINVAR_FASTER_WHISPER_MODEL", "tiny")
root = os.environ.get("CINVAR_FASTER_WHISPER_CACHE") or None
local_only = os.environ.get("CINVAR_FASTER_WHISPER_ALLOW_DOWNLOAD") != "1"
prompt = sys.argv[4] if len(sys.argv) > 4 and sys.argv[4] else None
hotwords = sys.argv[5] if len(sys.argv) > 5 and sys.argv[5] else None
m = WhisperModel(model, device="cpu", compute_type="int8", download_root=root, local_files_only=local_only)


def transcribe_with_prompt():
    """The prompt reaches the decoder through the first channel this build supports, so an
    older faster-whisper still runs instead of failing on an unknown kwarg."""
    channels = []
    if prompt:
        channels.append({"initial_prompt": prompt})
        if hotwords:
            channels.append({"initial_prompt": prompt, "hotwords": hotwords})
            channels.append({"hotwords": hotwords})
    channels.append({})
    last = None
    for extra in channels:
        try:
            return m.transcribe(audio, language=lang, vad_filter=True, **extra)
        except TypeError as exc:
            last = exc
    raise last


segments, info = transcribe_with_prompt()

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
