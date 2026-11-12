"""Reads an .otio file with the REAL OpenTimelineIO library and prints a JSON summary."""
import json, sys
import opentimelineio as otio

tl = otio.adapters.read_from_file(sys.argv[1])
out = {"name": tl.name, "tracks": []}
for tr in tl.tracks:
    items = []
    for ch in tr:
        if isinstance(ch, otio.schema.Clip):
            ref = ch.media_reference
            items.append({"type": "clip", "name": ch.name,
                          "start": ch.source_range.start_time.to_seconds(),
                          "duration": ch.source_range.duration.to_seconds(),
                          "target": getattr(ref, "target_url", None)})
        elif isinstance(ch, otio.schema.Gap):
            items.append({"type": "gap", "duration": ch.source_range.duration.to_seconds()})
        elif isinstance(ch, otio.schema.Transition):
            items.append({"type": "transition", "name": ch.transition_type,
                          "in": ch.in_offset.to_seconds(), "out": ch.out_offset.to_seconds()})
        else:
            items.append({"type": type(ch).__name__})
    out["tracks"].append({"name": tr.name, "kind": tr.kind, "items": items, "duration": tr.duration().to_seconds()})
if len(sys.argv) > 2:
    otio.adapters.write_to_file(tl, sys.argv[2])
print(json.dumps(out))
