//! Recordings to replay, read at run time from files the operator mounts (never built in).
//! Each frame is scrubbed again on reading, thinned to one frame per `frame_ms`, and kept as
//! compact JSON text, so several members can share one long recording cheaply.

use std::fs::{self, File};
use std::io::{self, BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sc_game::{Frame, parse_recording, scrub_frame};

pub struct Recording {
    /// `(ms from the first frame, frame as JSON)`, in time order.
    frames: Vec<(f64, Box<str>)>,
    duration_ms: f64,
}

impl Recording {
    /// Frames from `{ "t": ms, "frame": {...} }` lines, as Hardpoint records them.
    pub fn read(reader: impl BufRead, frame_ms: f64) -> io::Result<Self> {
        let mut frames: Vec<(f64, Box<str>)> = Vec::new();
        let mut start = None;
        for (n, line) in reader.lines().enumerate() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let parsed = parse_recording(&line).map_err(|e| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    e.to_string().replace("line 1", &format!("line {}", n + 1)),
                )
            })?;
            let Some(entry) = parsed.into_iter().next() else {
                continue;
            };
            let t = entry.t - *start.get_or_insert(entry.t);
            if frames.last().is_some_and(|(last, _)| t - last < frame_ms) {
                continue;
            }
            let text =
                serde_json::to_string(&scrub_frame(&entry.frame)).map_err(io::Error::other)?;
            frames.push((t, text.into_boxed_str()));
        }
        let Some(&(last, _)) = frames.last() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "the recording has no frames",
            ));
        };
        Ok(Self {
            frames,
            duration_ms: last + frame_ms,
        })
    }

    pub fn open(path: &Path, frame_ms: f64) -> io::Result<Self> {
        Self::read(BufReader::new(File::open(path)?), frame_ms)
            .map_err(|e| io::Error::new(e.kind(), format!("{}: {e}", path.display())))
    }

    pub fn len(&self) -> usize {
        self.frames.len()
    }

    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// How long one pass takes.
    pub fn duration_ms(&self) -> f64 {
        self.duration_ms
    }

    /// The frame playing `at_ms` into the recording, looping.
    pub fn frame_at(&self, at_ms: f64) -> Frame {
        let at = at_ms.rem_euclid(self.duration_ms);
        let i = self.frames.partition_point(|(t, _)| *t <= at).max(1) - 1;
        serde_json::from_str(&self.frames[i].1).unwrap_or(Frame::Null)
    }
}

/// The `.jsonl` files named, a folder standing for the `.jsonl` files in it, in name order.
pub fn files(paths: &[PathBuf]) -> io::Result<Vec<PathBuf>> {
    let mut out = Vec::new();
    for path in paths {
        if path.is_dir() {
            let mut found: Vec<PathBuf> = fs::read_dir(path)?
                .filter_map(|e| e.ok().map(|e| e.path()))
                .filter(|p| p.extension().is_some_and(|x| x == "jsonl"))
                .collect();
            found.sort();
            out.extend(found);
        } else {
            out.push(path.clone());
        }
    }
    if out.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "no recordings (.jsonl) found",
        ));
    }
    Ok(out)
}

pub fn load(paths: &[PathBuf], frame_ms: f64) -> io::Result<Vec<Arc<Recording>>> {
    files(paths)?
        .iter()
        .map(|p| Recording::open(p, frame_ms).map(Arc::new))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(t: u32, health: u32) -> String {
        format!(
            r#"{{"t":{t},"frame":{{"actor":{{"vitals":{{"health":{{"current":{health},"max":100}}}},"state":{{"uec_balance":"123456"}}}}}}}}"#
        )
    }

    #[test]
    fn frames_are_thinned_scrubbed_and_looped() {
        let text = [
            line(1000, 90),
            line(1100, 80),
            line(1600, 70),
            String::new(),
        ]
        .join("\n");
        let r = Recording::read(text.as_bytes(), 500.0).unwrap();
        assert_eq!(r.len(), 2);
        assert_eq!(r.duration_ms(), 1100.0);
        let health = |f: Frame| f["actor"]["vitals"]["health"]["current"].as_u64();
        assert_eq!(health(r.frame_at(0.0)), Some(90));
        assert_eq!(health(r.frame_at(700.0)), Some(70));
        assert_eq!(health(r.frame_at(1100.0 + 10.0)), Some(90));
        assert_eq!(r.frame_at(0.0)["actor"]["state"]["uec_balance"], "0");
    }

    #[test]
    fn a_bad_line_names_its_number() {
        let text = format!("{}\nnot json\n", line(0, 1));
        let e = Recording::read(text.as_bytes(), 500.0).err().unwrap();
        assert!(e.to_string().contains("line 2"), "{e}");
        assert!(Recording::read(&b""[..], 500.0).is_err());
    }
}
