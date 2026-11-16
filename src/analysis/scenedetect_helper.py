# CINVAR PySceneDetect helper: argv = video_path detector [threshold].
# detector is "content" (ContentDetector, default threshold 27.0) or "adaptive"
# (AdaptiveDetector, default adaptive_threshold 3.0). An empty threshold uses the default.
# Prints exactly one JSON object on stdout:
#   {"detector", "threshold", "fps", "width", "height",
#    "scenes": [{"start_frame", "start_sec", "end_frame", "end_sec"}]}
# end_frame / end_sec are exclusive (PySceneDetect's convention). Errors go to stderr, exit 2.
import json
import sys
import warnings

warnings.filterwarnings("ignore")

try:
    import scenedetect
except Exception as exc:  # ImportError or a broken install
    sys.stderr.write(
        "scenedetect is not importable in this Python (%s). "
        "Install scenedetect==0.7.3 into that environment and point "
        "CINVAR_SCENEDETECT_PYTHON at its python.\n" % exc
    )
    sys.exit(2)

if len(sys.argv) < 3:
    sys.stderr.write("usage: scenedetect_helper.py VIDEO content|adaptive [THRESHOLD]\n")
    sys.exit(2)

video, kind = sys.argv[1], sys.argv[2]
raw_threshold = sys.argv[3] if len(sys.argv) > 3 else ""

try:
    if kind == "content":
        threshold = float(raw_threshold) if raw_threshold else 27.0
        detector = scenedetect.ContentDetector(threshold=threshold, min_scene_len=15)
    elif kind == "adaptive":
        threshold = float(raw_threshold) if raw_threshold else 3.0
        detector = scenedetect.AdaptiveDetector(adaptive_threshold=threshold, min_scene_len=15)
    else:
        sys.stderr.write("unknown detector %r (use content or adaptive)\n" % kind)
        sys.exit(2)
except (TypeError, ValueError) as exc:
    sys.stderr.write("invalid threshold %r: %s\n" % (raw_threshold, exc))
    sys.exit(2)

try:
    stream = scenedetect.open_video(video, backend="opencv")
    fps = float(stream.frame_rate)
    width, height = (int(v) for v in stream.frame_size)
    scene_list = scenedetect.detect(video, detector, backend="opencv", show_progress=False)
except Exception as exc:
    sys.stderr.write("scene detection failed for %s: %s\n" % (video, exc))
    sys.exit(2)

scenes = [
    {
        "start_frame": int(start.frame_num),
        "start_sec": round(float(start.seconds), 6),
        "end_frame": int(end.frame_num),
        "end_sec": round(float(end.seconds), 6),
    }
    for start, end in scene_list
]
json.dump(
    {
        "detector": kind,
        "threshold": threshold,
        "fps": fps,
        "width": width,
        "height": height,
        "scenes": scenes,
    },
    sys.stdout,
)
sys.stdout.write("\n")
