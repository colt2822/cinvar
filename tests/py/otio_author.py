"""Authors a timeline with the REAL OpenTimelineIO library (not CINVAR) for import testing."""
import sys
import opentimelineio as otio

media = sys.argv[2]
tl = otio.schema.Timeline(name="authored-by-otio")
v = otio.schema.Track(name="V1", kind=otio.schema.TrackKind.Video)
a = otio.schema.Track(name="A1", kind=otio.schema.TrackKind.Audio)
rate = 24.0
def ref(): return otio.schema.ExternalReference(target_url=media)
c1 = otio.schema.Clip(name="one", media_reference=ref(), source_range=otio.opentime.TimeRange(otio.opentime.RationalTime(12, rate), otio.opentime.RationalTime(24, rate)))
gap = otio.schema.Gap(source_range=otio.opentime.TimeRange(otio.opentime.RationalTime(0, rate), otio.opentime.RationalTime(12, rate)))
c2 = otio.schema.Clip(name="two", media_reference=ref(), source_range=otio.opentime.TimeRange(otio.opentime.RationalTime(0, rate), otio.opentime.RationalTime(24, rate)))
v.extend([c1, gap, c2])
a1 = otio.schema.Clip(name="aone", media_reference=ref(), source_range=otio.opentime.TimeRange(otio.opentime.RationalTime(0, rate), otio.opentime.RationalTime(48, rate)))
a.append(a1)
tl.tracks.extend([v, a])
otio.adapters.write_to_file(tl, sys.argv[1])
