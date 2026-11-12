use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

pub const NANOS_PER_SEC: i64 = 1_000_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Rational {
    pub num: u32,
    pub den: u32,
}

impl Rational {
    pub const fn new(num: u32, den: u32) -> Self {
        Rational { num, den }
    }
    pub fn as_f64(self) -> f64 {
        self.num as f64 / self.den as f64
    }
    pub fn is_valid(self) -> bool {
        self.num > 0 && self.den > 0
    }
    pub fn ffprobe_fraction(self) -> String {
        format!("{}/{}", self.num, self.den)
    }
}

impl Default for Rational {
    fn default() -> Self {
        Rational::new(25, 1)
    }
}

pub type Fps = Rational;

pub fn fps_from_f64(v: f64) -> Fps {
    const COMMON: [(u32, u32); 12] = [
        (24000, 1001),
        (24, 1),
        (25, 1),
        (30000, 1001),
        (30, 1),
        (48000, 1001),
        (50, 1),
        (60000, 1001),
        (60, 1),
        (120, 1),
        (25_000, 1_000),
        (50_000, 1_000),
    ];
    for (num, den) in COMMON {
        let f = num as f64 / den as f64;
        if (f - v).abs() < 0.005 {
            return Rational::new(num, den);
        }
    }
    Rational::new((v * 1000.0).round() as u32, 1000)
}

pub fn parse_fps(s: &str) -> Result<Fps> {
    let s = s.trim();
    if s.is_empty() {
        return Err(Error::invalid("empty frame rate"));
    }
    if let Some((a, b)) = s.split_once('/') {
        let num: u32 = a
            .trim()
            .parse()
            .map_err(|_| Error::invalid(format!("bad frame rate '{s}'")))?;
        let den: u32 = b
            .trim()
            .parse()
            .map_err(|_| Error::invalid(format!("bad frame rate '{s}'")))?;
        if num == 0 || den == 0 {
            return Err(Error::invalid(format!("bad frame rate '{s}'")));
        }
        return Ok(Rational::new(num, den));
    }
    let v: f64 = s
        .parse()
        .map_err(|_| Error::invalid(format!("bad frame rate '{s}'")))?;
    if !(v > 0.0 && v < 1000.0) {
        return Err(Error::invalid(format!("frame rate out of range: '{s}'")));
    }
    Ok(fps_from_f64(v))
}

#[derive(
    Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default, Serialize, Deserialize,
)]
#[serde(transparent)]
pub struct Time {
    pub nanos: i64,
}

impl Time {
    pub const ZERO: Time = Time { nanos: 0 };

    pub const fn from_nanos(nanos: i64) -> Self {
        Time { nanos }
    }
    pub const fn from_micros(us: i64) -> Self {
        Time { nanos: us * 1_000 }
    }
    pub const fn from_millis(ms: i64) -> Self {
        Time {
            nanos: ms * 1_000_000,
        }
    }
    pub fn from_secs_f64(s: f64) -> Self {
        Time {
            nanos: (s * NANOS_PER_SEC as f64).round() as i64,
        }
    }
    pub fn from_frames(frames: i64, fps: Fps) -> Self {
        let den = fps.den as i128;
        let num = fps.num as i128;
        let nanos = (frames as i128 * den * NANOS_PER_SEC as i128) / num;
        Time {
            nanos: nanos as i64,
        }
    }
    pub fn to_frames(self, fps: Fps) -> i64 {
        let num = fps.num as i128;
        let den = fps.den as i128;
        let scaled = self.nanos as i128 * num;
        let base = NANOS_PER_SEC as i128 * den;
        let q = scaled / base;
        let r = scaled % base;
        let q = if r.abs() * 2 >= base.abs() {
            q + r.signum()
        } else {
            q
        };
        q as i64
    }
    pub fn as_secs_f64(self) -> f64 {
        self.nanos as f64 / NANOS_PER_SEC as f64
    }
    pub fn max(self, other: Time) -> Time {
        if self.nanos >= other.nanos {
            self
        } else {
            other
        }
    }
    pub fn min(self, other: Time) -> Time {
        if self.nanos <= other.nanos {
            self
        } else {
            other
        }
    }
    pub fn is_negative(self) -> bool {
        self.nanos < 0
    }
    pub fn clamp_non_negative(self) -> Time {
        Time {
            nanos: self.nanos.max(0),
        }
    }
    pub fn scale(self, factor: f64) -> Time {
        Time {
            nanos: (self.nanos as f64 * factor).round() as i64,
        }
    }
    pub fn ffmpeg_timestamp(self) -> String {
        let total = self.nanos.max(0) as f64 / NANOS_PER_SEC as f64;
        let h = (total / 3600.0).floor() as i64;
        let m = ((total - h as f64 * 3600.0) / 60.0).floor() as i64;
        let s = total - (h * 3600 + m * 60) as f64;
        format!("{h:02}:{m:02}:{s:06.3}")
    }
}

impl std::fmt::Display for Time {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.ffmpeg_timestamp())
    }
}

impl std::str::FromStr for Time {
    type Err = Error;
    fn from_str(s: &str) -> Result<Time> {
        let raw = s.trim();
        if raw.is_empty() {
            return Err(Error::invalid("empty time value"));
        }
        let lower = raw.to_ascii_lowercase();
        if let Some(v) = lower.strip_suffix("ms") {
            let n: f64 = v
                .trim()
                .parse()
                .map_err(|_| Error::invalid(format!("bad time '{raw}'")))?;
            return Ok(Time::from_millis(n.round() as i64));
        }
        if let Some(v) = lower.strip_suffix('s') {
            if !v.contains(':') {
                let n: f64 = v
                    .trim()
                    .parse()
                    .map_err(|_| Error::invalid(format!("bad time '{raw}'")))?;
                return Ok(Time::from_secs_f64(n));
            }
        }
        let parts: Vec<&str> = lower.split(':').collect();
        let to_f = |p: &str| -> Result<f64> {
            p.trim()
                .parse::<f64>()
                .map_err(|_| Error::invalid(format!("bad time '{raw}'")))
        };
        let secs = match parts.len() {
            1 => to_f(parts[0])?,
            2 => to_f(parts[0])? * 60.0 + to_f(parts[1])?,
            3 => to_f(parts[0])? * 3600.0 + to_f(parts[1])? * 60.0 + to_f(parts[2])?,
            _ => return Err(Error::invalid(format!("bad time '{raw}'"))),
        };
        if secs < 0.0 {
            return Err(Error::invalid(format!("negative time '{raw}'")));
        }
        Ok(Time::from_secs_f64(secs))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: Time,
    pub end: Time,
}

impl TimeRange {
    pub fn new(start: Time, end: Time) -> Self {
        TimeRange { start, end }
    }
    pub fn duration(self) -> Time {
        (self.end - self.start).clamp_non_negative()
    }
    pub fn overlaps(self, other: TimeRange) -> bool {
        self.start < other.end && other.start < self.end
    }
    pub fn contains(self, t: Time) -> bool {
        self.start <= t && t < self.end
    }
}

impl std::ops::Add for Time {
    type Output = Time;
    fn add(self, other: Time) -> Time {
        Time {
            nanos: self.nanos.saturating_add(other.nanos),
        }
    }
}

impl std::ops::Sub for Time {
    type Output = Time;
    fn sub(self, other: Time) -> Time {
        Time {
            nanos: self.nanos.saturating_sub(other.nanos),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_roundtrip_ntsc() {
        let fps = parse_fps("30000/1001").unwrap();
        for frame in [0i64, 1, 29, 30, 1000, 2997, 17982] {
            let t = Time::from_frames(frame, fps);
            assert_eq!(t.to_frames(fps), frame, "frame {frame} roundtrip");
        }
    }

    #[test]
    fn frame_roundtrip_25() {
        let fps = parse_fps("25").unwrap();
        for frame in [0i64, 1, 24, 25, 12345] {
            assert_eq!(Time::from_frames(frame, fps).to_frames(fps), frame);
        }
    }

    #[test]
    fn parse_various_times() {
        assert_eq!("1:30".parse::<Time>().unwrap().nanos, 90 * NANOS_PER_SEC);
        assert_eq!("00:00:02.500".parse::<Time>().unwrap().nanos, 2_500_000_000);
        assert_eq!("1500ms".parse::<Time>().unwrap().nanos, 1_500_000_000);
        assert_eq!("2.5s".parse::<Time>().unwrap().nanos, 2_500_000_000);
        assert!("nonsense".parse::<Time>().is_err());
    }

    #[test]
    fn fps_parsing() {
        assert_eq!(parse_fps("29.97").unwrap(), Rational::new(30000, 1001));
        assert_eq!(parse_fps("23.976").unwrap(), Rational::new(24000, 1001));
        assert_eq!(parse_fps("30").unwrap(), Rational::new(30, 1));
        assert_eq!(parse_fps("30000/1001").unwrap(), Rational::new(30000, 1001));
        assert!(parse_fps("0").is_err());
    }

    #[test]
    fn range_overlap() {
        let a = TimeRange::new(Time::from_secs_f64(0.0), Time::from_secs_f64(2.0));
        let b = TimeRange::new(Time::from_secs_f64(1.0), Time::from_secs_f64(3.0));
        let c = TimeRange::new(Time::from_secs_f64(2.0), Time::from_secs_f64(4.0));
        assert!(a.overlaps(b));
        assert!(!a.overlaps(c));
    }
}
