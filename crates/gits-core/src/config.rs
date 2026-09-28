//! Configuration. `config/default.toml` (compiled in) holds every default; a
//! user file is deep-merged over it, so both the defaults and the documentation
//! live in one place inside the repository.

use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub const DEFAULT_TOML: &str = include_str!("../../../config/default.toml");

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub palette: Palette,
    pub events: Events,
    pub age: Age,
    pub calm: Calm,
    pub tower: Tower,
    pub labels: Labels,
    pub background: Background,
    pub camera: Camera,
    pub monitors: BTreeMap<String, Monitor>,
    pub performance: Performance,
    pub privacy: Privacy,
    pub data: Data,
    pub collector: Collector,
    pub synthetic: Synthetic,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Palette {
    pub background: String,
    pub gold: String,
    pub cream: String,
    pub orange: String,
    pub crimson: String,
    pub teal: String,
    pub exposure: f32,
    pub bloom: f32,
    pub grain: f32,
    pub vignette: f32,
    pub smear: Dur,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Events {
    pub read: ReadStyle,
    pub write: WriteStyle,
    pub create: CreateStyle,
    pub delete: DeleteStyle,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ReadStyle {
    pub color: String,
    pub decay: Dur,
    pub intensity: f32,
    pub probe: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct WriteStyle {
    pub color: String,
    pub decay: Dur,
    pub intensity: f32,
    pub spin_up: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct CreateStyle {
    pub color: String,
    pub grow: Dur,
    pub intensity: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct DeleteStyle {
    pub color: String,
    pub intensity: f32,
    pub fall: bool,
    pub ghost_hours: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Age {
    pub fresh: String,
    pub old: String,
    pub stale: String,
    pub fresh_days: f32,
    pub old_days: f32,
    pub stale_days: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Calm {
    pub max_flash_hz: f32,
    pub attack: Dur,
    pub spin_speed: f32,
    pub activity_decay: Dur,
    pub max_open_dirs: usize,
    pub surge_hud: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Tower {
    pub height: f32,
    pub radius: f32,
    pub inner_radius: f32,
    pub depth_layers: u32,
    pub min_arc: f32,
    pub max_arc: f32,
    pub gap: f32,
    pub dir_marks: bool,
    pub open_pitch: f32,
    pub ribbon: f32,
    pub fill: f32,
    pub fill_open: f32,
    pub idle_wound: f32,
    pub idle_open: f32,
    pub back_face: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Labels {
    pub max: usize,
    pub hold: Dur,
    pub repeat: Dur,
    pub per_second: f32,
    pub reads: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Background {
    pub calligraphy: bool,
    pub calligraphy_brightness: f32,
    pub lattice: bool,
    pub lattice_brightness: f32,
    pub floor_lights: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Camera {
    pub drift: f32,
    pub rotate_compositions: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Monitor {
    pub composition: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Performance {
    pub fps: u32,
    pub fps_battery: u32,
    /// "auto", "discrete" or "integrated".
    pub gpu: String,
    pub render_scale: f32,
    /// 0 = no cap.
    pub max_internal_megapixels: f32,
    pub integrated_render_scale: f32,
    pub integrated_max_megapixels: f32,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Privacy {
    pub text: String,
    pub obfuscate_when_sharing: bool,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Data {
    /// "auto", "collector" or "synthetic".
    pub source: String,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Collector {
    pub roots: Vec<String>,
    pub excludes: Vec<String>,
    pub max_rings: usize,
    /// inotify watches to use; 0 = automatic.
    pub max_watches: usize,
    pub crawl_interval: Dur,
}

#[derive(Clone, Debug, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Synthetic {
    pub seed: u64,
    pub scale: f32,
    pub speed: f32,
    /// Loop length in simulated seconds (0 = no loop).
    #[serde(rename = "loop")]
    pub loop_len: Dur,
}

/// A duration in seconds, parsed from a number or a string like "150ms", "4m".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Dur(pub f32);

impl<'de> Deserialize<'de> for Dur {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Int(i64),
            Float(f64),
            Str(String),
        }
        match Raw::deserialize(d)? {
            Raw::Int(n) => Ok(Dur(n as f32)),
            Raw::Float(n) => Ok(Dur(n as f32)),
            Raw::Str(s) => parse_duration(&s).map(Dur).map_err(serde::de::Error::custom),
        }
    }
}

pub fn parse_duration(s: &str) -> std::result::Result<f32, String> {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let n: f32 = num.trim().parse().map_err(|_| format!("bad duration '{s}'"))?;
    let mult = match unit.trim() {
        "" | "s" => 1.0,
        "ms" => 0.001,
        "m" | "min" => 60.0,
        "h" => 3600.0,
        "d" => 86400.0,
        u => return Err(format!("unknown duration unit '{u}' in '{s}'")),
    };
    Ok(n * mult)
}

impl Performance {
    /// Whether to render on a discrete GPU. `on_battery` is only consulted for "auto".
    pub fn wants_discrete(&self, on_battery: bool) -> bool {
        match self.gpu.as_str() {
            "discrete" => true,
            "integrated" => false,
            _ => !on_battery,
        }
    }

    /// (render scale, megapixel cap or f32::INFINITY) for the kind of GPU in use.
    pub fn resolution(&self, integrated: bool) -> (f32, f32) {
        let (s, mp) = if integrated {
            (self.integrated_render_scale, self.integrated_max_megapixels)
        } else {
            (self.render_scale, self.max_internal_megapixels)
        };
        (s.clamp(0.2, 1.0), if mp > 0.0 { mp } else { f32::INFINITY })
    }
}

impl Default for Config {
    fn default() -> Self {
        toml::from_str(DEFAULT_TOML).expect("built-in config/default.toml must be valid")
    }
}

impl Config {
    /// `~/.config/gits-file-visualizer/config.toml` (respecting `XDG_CONFIG_HOME`).
    pub fn user_path() -> Option<PathBuf> {
        let base = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .filter(|p| p.is_absolute())
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
        Some(base.join("gits-file-visualizer").join("config.toml"))
    }

    /// Defaults merged with the given user TOML text.
    pub fn from_user_toml(user: &str) -> Result<Self> {
        let mut base: toml::Table = toml::from_str(DEFAULT_TOML).context("built-in defaults")?;
        let over: toml::Table = toml::from_str(user).context("parsing user config")?;
        merge(&mut base, over);
        let cfg: Config = toml::Value::Table(base).try_into().context("invalid config value")?;
        Ok(cfg)
    }

    /// Loads defaults merged with `path` if it exists.
    pub fn load(path: Option<&Path>) -> Result<Self> {
        match path {
            Some(p) if p.exists() => {
                let text = std::fs::read_to_string(p).with_context(|| format!("reading {}", p.display()))?;
                Self::from_user_toml(&text).with_context(|| format!("in {}", p.display()))
            }
            _ => Ok(Self::default()),
        }
    }

    pub fn monitor(&self, connector: &str) -> Monitor {
        self.monitors.get(connector).or_else(|| self.monitors.get("*")).cloned().unwrap_or(Monitor { composition: "wide".into() })
    }

    /// Resolves a palette name or hex string to linear RGB.
    pub fn color(&self, name: &str) -> [f32; 3] {
        let p = &self.palette;
        let hex = match name.trim() {
            "background" => p.background.as_str(),
            "gold" => p.gold.as_str(),
            "cream" => p.cream.as_str(),
            "orange" => p.orange.as_str(),
            "crimson" => p.crimson.as_str(),
            "teal" => p.teal.as_str(),
            other => other,
        };
        parse_hex(hex).unwrap_or_else(|| {
            log::warn!("unknown colour '{name}', using gold");
            parse_hex(&p.gold).unwrap_or([1.0, 0.45, 0.02])
        })
    }
}

fn merge(base: &mut toml::Table, over: toml::Table) {
    for (k, v) in over {
        match (base.get_mut(&k), v) {
            (Some(toml::Value::Table(b)), toml::Value::Table(o)) => merge(b, o),
            (_, v) => {
                base.insert(k, v);
            }
        }
    }
}

/// "#rrggbb" (sRGB) → linear RGB.
pub fn parse_hex(s: &str) -> Option<[f32; 3]> {
    let s = s.trim().trim_start_matches('#');
    if s.len() != 6 {
        return None;
    }
    let v = u32::from_str_radix(s, 16).ok()?;
    let c = |shift: u32| srgb_to_linear(((v >> shift) & 0xff) as f32 / 255.0);
    Some([c(16), c(8), c(0)])
}

pub fn srgb_to_linear(c: f32) -> f32 {
    if c <= 0.04045 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_parse() {
        let c = Config::default();
        assert_eq!(c.performance.fps, 60);
        assert!((c.events.write.decay.0 - 900.0).abs() < 1e-3);
    }

    #[test]
    fn user_merge_overrides_only_given_keys() {
        let c = Config::from_user_toml("[events.write]\ncolor = \"#00ff00\"\n").unwrap();
        assert_eq!(c.events.write.color, "#00ff00");
        assert!((c.events.write.decay.0 - 900.0).abs() < 1e-3);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(Config::from_user_toml("[tower]\nhieght = 3.0\n").is_err());
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("150ms").unwrap(), 0.15);
        assert_eq!(parse_duration("4m").unwrap(), 240.0);
        assert_eq!(parse_duration("2").unwrap(), 2.0);
        assert!(parse_duration("3 fortnights").is_err());
    }
}
