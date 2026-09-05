//! Spool-and-forward for measurements captured while Postgres is unreachable.
//!
//! Lines are `{received_at RFC3339}\t{rssi|empty}\t{frame hex}`. The file is
//! truncated only after a successful drain; the DB's dedup constraint makes a
//! crash between read and delete harmless.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;

use chrono::{DateTime, FixedOffset};

#[derive(Debug, Clone)]
pub struct SpooledFrame {
    pub received_at: DateTime<FixedOffset>,
    pub rssi: Option<i16>,
    pub payload: Vec<u8>,
}

#[derive(Debug, Default)]
pub struct DrainResult {
    pub frames: Vec<SpooledFrame>,
    pub malformed: usize,
}

pub struct Spool {
    path: PathBuf,
    max_bytes: u64,
}

impl Spool {
    pub fn new(path: PathBuf, max_bytes: u64) -> Spool {
        Spool { path, max_bytes }
    }

    pub fn append(
        &self,
        payload: &[u8],
        received_at: DateTime<FixedOffset>,
        rssi: Option<i16>,
    ) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let line = format!(
            "{}\t{}\t{}\n",
            received_at.to_rfc3339(),
            rssi.map(|v| v.to_string()).unwrap_or_default(),
            hex::encode(payload),
        );
        self.enforce_cap()?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        file.write_all(line.as_bytes())
    }

    /// Read and remove all spooled frames.
    pub fn drain(&self) -> io::Result<DrainResult> {
        let mut result = DrainResult::default();
        let Ok(content) = fs::read_to_string(&self.path) else {
            return Ok(result);
        };
        for line in content.lines().filter(|line| !line.trim().is_empty()) {
            match parse_line(line) {
                Some(frame) => result.frames.push(frame),
                None => result.malformed += 1,
            }
        }
        match fs::remove_file(&self.path) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        Ok(result)
    }

    /// Drop the oldest lines until the file is at half capacity.
    fn enforce_cap(&self) -> io::Result<()> {
        let Ok(metadata) = fs::metadata(&self.path) else {
            return Ok(());
        };
        if metadata.len() <= self.max_bytes {
            return Ok(());
        }
        let content = fs::read_to_string(&self.path)?;
        let mut lines: Vec<&str> = content.lines().collect();
        let budget = self.max_bytes / 2;
        let mut size = content.len();
        while size > budget as usize && !lines.is_empty() {
            size -= lines.remove(0).len() + 1;
        }
        let mut file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&self.path)?;
        for line in &lines {
            writeln!(file, "{line}")?;
        }
        Ok(())
    }
}

/// Parse one spool line: `{received_at RFC3339}\t{rssi|empty}\t{frame hex}`.
///
/// Public because `grammatic replay` consumes the same format — the spool owns
/// it, nobody re-parses it by hand.
pub fn parse_line(line: &str) -> Option<SpooledFrame> {
    let mut parts = line.splitn(3, '\t');
    let received_at = DateTime::parse_from_rfc3339(parts.next()?).ok()?;
    let rssi = parts.next()?.trim().parse::<i16>().ok();
    let payload = hex::decode(parts.next()?).ok()?;
    (!payload.is_empty()).then_some(SpooledFrame {
        received_at,
        rssi,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone};

    fn temp_spool(name: &str) -> Spool {
        let path = std::env::temp_dir().join(format!("grammatic-test-{name}-{}", std::process::id()));
        let _ = fs::remove_file(&path);
        Spool::new(path, 4096)
    }

    fn at(offset_secs: i64) -> DateTime<FixedOffset> {
        let base = FixedOffset::east_opt(2 * 3600)
            .unwrap()
            .with_ymd_and_hms(2026, 9, 3, 12, 0, 0)
            .unwrap();
        base + Duration::seconds(offset_secs)
    }

    fn frame(weight_raw: u16) -> Vec<u8> {
        let mut frame = vec![0u8; 13];
        frame[0] = 0x02;
        frame[1] = 0x22;
        frame[11] = (weight_raw & 0xFF) as u8;
        frame[12] = (weight_raw >> 8) as u8;
        frame
    }

    #[test]
    fn append_and_drain_roundtrip() {
        let spool = temp_spool("roundtrip");
        spool.append(&frame(15000), at(0), Some(-55)).unwrap();
        spool.append(&frame(15010), at(1), None).unwrap();
        let drained = spool.drain().unwrap();
        assert_eq!(drained.malformed, 0);
        assert_eq!(drained.frames.len(), 2);
        assert_eq!(drained.frames[0].payload, frame(15000));
        assert_eq!(drained.frames[0].rssi, Some(-55));
        assert_eq!(drained.frames[1].rssi, None);
        assert!(!spool.path.exists());
        // Draining again yields nothing.
        assert!(spool.drain().unwrap().frames.is_empty());
    }

    #[test]
    fn cap_drops_oldest_lines() {
        let spool = temp_spool("cap");
        let mut small = Spool::new(spool.path.clone(), 200);
        small.max_bytes = 200;
        for i in 0..20 {
            small
                .append(&frame(15000 + i as u16), at(i as i64), None)
                .unwrap();
        }
        let drained = small.drain().unwrap();
        assert!(
            drained.frames.len() < 20,
            "cap should have dropped oldest lines"
        );
        assert_eq!(drained.frames.last().unwrap().received_at, at(19));
    }

    #[test]
    fn malformed_lines_are_counted() {
        let spool = temp_spool("malformed");
        fs::write(&spool.path, "not-a-line\n").unwrap();
        let drained = spool.drain().unwrap();
        assert_eq!(drained.frames.len(), 0);
        assert_eq!(drained.malformed, 1);
        assert!(!spool.path.exists());
    }

    #[test]
    fn parse_roundtrips_negative_rssi() {
        let line = format!("{}\t-55\t{}", at(5).to_rfc3339(), hex::encode(frame(1)));
        let parsed = parse_line(&line).unwrap();
        assert_eq!(parsed.rssi, Some(-55));
    }
}
