//! Valores con unidad que aparecen en los archivos TOML: duraciones (`5m`, `60s`)
//! y tamaños (`500MB`).

use std::fmt;
use std::str::FromStr;
use std::time::Duration;

use serde::de::{self, Deserialize, Deserializer};

/// Duración escrita a mano en el TOML (`30s`, `5m`, `1h 30m`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Dur(pub Duration);

impl Dur {
    pub fn as_duration(self) -> Duration {
        self.0
    }
}

impl FromStr for Dur {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        humantime::parse_duration(s.trim())
            .map(Dur)
            .map_err(|_| format!("duración inválida '{s}' (ejemplos: 30s, 5m, 1h)"))
    }
}

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(de::Error::custom)
    }
}

/// Duración legible para escribir en un TOML: `45s`, `5m`, `2h`, `500ms` (se lee de vuelta con
/// [`Dur`]). `90s` se queda en segundos: no se redondea nada.
pub fn format_duration(d: Duration) -> String {
    let ms = d.as_millis();
    if !ms.is_multiple_of(1000) {
        return format!("{ms}ms");
    }
    let s = d.as_secs();
    if s >= 3600 && s.is_multiple_of(3600) {
        format!("{}h", s / 3600)
    } else if s >= 60 && s.is_multiple_of(60) {
        format!("{}m", s / 60)
    } else {
        format!("{s}s")
    }
}

/// Tamaño en bytes escrito con sufijo (`500MB`, `1.5GB`, `64KiB`).
///
/// `KB`, `MB`, `GB`, `TB` son decimales (1000); `KiB`, `MiB`, `GiB`, `TiB` son binarios (1024).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteSize(pub u64);

impl ByteSize {
    pub fn bytes(self) -> u64 {
        self.0
    }
}

impl FromStr for ByteSize {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let invalid = || format!("tamaño inválido '{s}' (ejemplos: 500MB, 2GB, 64KiB)");
        let s = s.trim();
        let split = s
            .find(|c: char| !(c.is_ascii_digit() || c == '.'))
            .unwrap_or(s.len());
        let (num, unit) = s.split_at(split);
        let num: f64 = num.parse().map_err(|_| invalid())?;
        let mult: f64 = match unit.trim().to_ascii_lowercase().as_str() {
            "" | "b" => 1.0,
            "kb" => 1e3,
            "mb" => 1e6,
            "gb" => 1e9,
            "tb" => 1e12,
            "kib" => 1024.0,
            "mib" => 1024.0 * 1024.0,
            "gib" => 1024.0 * 1024.0 * 1024.0,
            "tib" => 1024.0 * 1024.0 * 1024.0 * 1024.0,
            _ => return Err(invalid()),
        };
        Ok(ByteSize((num * mult).round() as u64))
    }
}

impl fmt::Display for ByteSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let b = self.0 as f64;
        if b >= 1e9 {
            write!(f, "{:.1} GB", b / 1e9)
        } else if b >= 1e6 {
            write!(f, "{:.0} MB", b / 1e6)
        } else if b >= 1e3 {
            write!(f, "{:.0} KB", b / 1e3)
        } else {
            write!(f, "{} B", self.0)
        }
    }
}

impl<'de> Deserialize<'de> for ByteSize {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let s = String::deserialize(d)?;
        s.parse().map_err(de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn durations() {
        assert_eq!("5m".parse::<Dur>().unwrap().0, Duration::from_secs(300));
        assert_eq!("60s".parse::<Dur>().unwrap().0, Duration::from_secs(60));
        assert_eq!(
            "1h 30m".parse::<Dur>().unwrap().0,
            Duration::from_secs(5400)
        );
        assert!("5 minutos".parse::<Dur>().is_err());
        assert!("".parse::<Dur>().is_err());
    }

    #[test]
    fn formats_durations_readably_and_they_read_back() {
        let d = |s| format_duration(Duration::from_secs(s));
        assert_eq!(d(45), "45s");
        assert_eq!(d(60), "1m");
        assert_eq!(d(300), "5m");
        assert_eq!(d(90), "90s");
        assert_eq!(d(3600), "1h");
        assert_eq!(d(3660), "61m");
        assert_eq!(format_duration(Duration::from_millis(500)), "500ms");
        for secs in [1, 45, 60, 90, 300, 3600, 3660, 86400] {
            let text = d(secs);
            assert_eq!(
                text.parse::<Dur>().unwrap().0,
                Duration::from_secs(secs),
                "{text}"
            );
        }
    }

    #[test]
    fn sizes() {
        assert_eq!("500MB".parse::<ByteSize>().unwrap().0, 500_000_000);
        assert_eq!("500 mb".parse::<ByteSize>().unwrap().0, 500_000_000);
        assert_eq!("1.5GB".parse::<ByteSize>().unwrap().0, 1_500_000_000);
        assert_eq!("64KiB".parse::<ByteSize>().unwrap().0, 65_536);
        assert_eq!("10".parse::<ByteSize>().unwrap().0, 10);
        assert!("MB".parse::<ByteSize>().is_err());
        assert!("5 parsecs".parse::<ByteSize>().is_err());
    }

    #[test]
    fn size_display() {
        assert_eq!(ByteSize(412_000_000).to_string(), "412 MB");
        assert_eq!(ByteSize(2_500_000_000).to_string(), "2.5 GB");
    }
}
