//! Parsing and formatting helpers shared across modules.

use std::fs::Metadata;

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

use chrono::Duration;
use humansize::{format_size, BINARY};

use crate::errors::ParseError;

/// Bytes physically allocated on disk for this entry (0-byte files may
/// occupy no blocks; sparse files report only their real usage).
#[must_use]
pub fn physical_disk_size(metadata: &Metadata) -> u64 {
    #[cfg(unix)]
    {
        metadata.blocks() * 512
    }
    #[cfg(not(unix))]
    {
        metadata.len()
    }
}

/// Platform device identifier used to detect mount-point crossings.
/// Returns `None` on platforms without the concept.
#[must_use]
pub fn device_id(metadata: &Metadata) -> Option<u64> {
    #[cfg(unix)]
    {
        Some(metadata.dev())
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        None
    }
}

/// Parse a human-readable byte size such as `"512"`, `"100K"` or `"1.5G"`
/// into an exact byte count. Units are interpreted as binary multiples:
/// 1 K = 1024 bytes, 1 M = 1024 * 1024 bytes, and so on.
pub fn parse_size(input: &str) -> Result<u64, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::InvalidByteSize {
            input: input.to_owned(),
            reason: "value is empty".to_owned(),
        });
    }

    let split_at = first_alphabetic(trimmed);
    let (number, unit) = trimmed.split_at(split_at);

    let value = parse_number(number).map_err(|reason| ParseError::InvalidByteSize {
        input: input.to_owned(),
        reason: reason.to_owned(),
    })?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1_u64,
        "k" | "kb" | "kib" => 1024,
        "m" | "mb" | "mib" => 1024 * 1024,
        "g" | "gb" | "gib" => 1024 * 1024 * 1024,
        "t" | "tb" | "tib" => 1024_u64.pow(4),
        "p" | "pb" | "pib" => 1024_u64.pow(5),
        other => {
            return Err(ParseError::InvalidByteSize {
                input: input.to_owned(),
                reason: format!(
                    "unknown unit {other:?}; expected B, K, M, G, T or P (optionally KiB/MiB/GiB/TiB/PiB)"
                ),
            })
        }
    };

    let bytes = value * multiplier as f64;
    if bytes > u64::MAX as f64 {
        return Err(ParseError::InvalidByteSize {
            input: input.to_owned(),
            reason: "value exceeds the 64-bit range".to_owned(),
        });
    }
    Ok(bytes as u64)
}

/// Parse a duration string such as `"45s"`, `"12h"`, `"30d"` or `"2w"`
/// into a [`chrono::Duration`]. Supported units: s, m, h, d, w, mo (30d), y (365d).
pub fn parse_duration(input: &str) -> Result<Duration, ParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Err(ParseError::InvalidDuration {
            input: input.to_owned(),
            reason: "value is empty".to_owned(),
        });
    }

    let lowered = trimmed.to_ascii_lowercase();
    let split_at = first_alphabetic(&lowered);
    if split_at == lowered.len() {
        return Err(ParseError::InvalidDuration {
            input: input.to_owned(),
            reason: "missing unit; expected e.g. \"30d\", \"12h\" or \"45m\"".to_owned(),
        });
    }

    let (number, unit) = lowered.split_at(split_at);
    let value = parse_number(number).map_err(|reason| ParseError::InvalidDuration {
        input: input.to_owned(),
        reason: reason.to_owned(),
    })?;

    const MAX_SECS: f64 = (i64::MAX / 1000) as f64;
    let seconds = value
        * match unit {
            "s" | "sec" | "secs" | "second" | "seconds" => 1.0,
            "m" | "min" | "mins" | "minute" | "minutes" => 60.0,
            "h" | "hr" | "hrs" | "hour" | "hours" => 3_600.0,
            "d" | "day" | "days" => 86_400.0,
            "w" | "week" | "weeks" => 604_800.0,
            "mo" | "month" | "months" => 2_592_000.0,
            "y" | "year" | "years" => 31_536_000.0,
            other => {
                return Err(ParseError::InvalidDuration {
                    input: input.to_owned(),
                    reason: format!("unknown unit {other:?}; expected s, m, h, d, w, mo or y"),
                })
            }
        };

    if !seconds.is_finite() || !(0.0..=MAX_SECS).contains(&seconds) {
        return Err(ParseError::InvalidDuration {
            input: input.to_owned(),
            reason: "duration must be non-negative and within the supported range".to_owned(),
        });
    }

    Ok(Duration::seconds(seconds as i64))
}

/// Render a byte count in human-readable binary units (e.g. `1.50 MiB`).
pub fn format_bytes(bytes: u64) -> String {
    format_size(bytes, BINARY)
}

/// Render a duration compactly (e.g. `1d 2h 30m`).
pub fn format_duration(duration: &Duration) -> String {
    let total = duration.num_seconds().max(0);
    let days = total / 86_400;
    let hours = (total % 86_400) / 3_600;
    let minutes = (total % 3_600) / 60;
    let seconds = total % 60;

    let mut parts = Vec::new();
    if days > 0 {
        parts.push(format!("{days}d"));
    }
    if hours > 0 {
        parts.push(format!("{hours}h"));
    }
    if minutes > 0 {
        parts.push(format!("{minutes}m"));
    }
    if seconds > 0 || parts.is_empty() {
        parts.push(format!("{seconds}s"));
    }
    parts.join(" ")
}

fn first_alphabetic(text: &str) -> usize {
    text.char_indices()
        .find(|(_, c)| c.is_ascii_alphabetic())
        .map(|(idx, _)| idx)
        .unwrap_or(text.len())
}

fn parse_number(number: &str) -> Result<f64, &'static str> {
    let parsed: f64 = number
        .trim()
        .parse()
        .map_err(|_| "expected a leading number")?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err("must be a non-negative finite number");
    }
    Ok(parsed)
}
