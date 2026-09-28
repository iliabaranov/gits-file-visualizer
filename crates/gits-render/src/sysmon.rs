//! A small system readout for the main screen: CPU, memory, GPU, power and
//! network, each with a one-minute sparkline. A background thread samples once
//! a second from /proc and /sys (and NVIDIA's NVML library, loaded at run time,
//! when rendering on an NVIDIA GPU). Nothing is sampled while the readout is
//! not on screen.

use crate::fx::{line_px, quad_px, text_px};
use crate::gpu_types::SpriteVertex;
use crate::text::GlyphAtlas;
use crate::world::smoothstep;
use gits_core::config::Config;
use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Seconds of history in the sparklines.
const HISTORY_S: f64 = 60.0;
const ROWS: [&str; 5] = ["CPU", "MEM", "GPU", "PWR", "NET"];
// Layout at 1080p (scaled with the screen).
const MARGIN_R: f32 = 32.0;
const MARGIN_B: f32 = 34.0;
const PITCH: f32 = 20.0;
const WIDTH: f32 = 300.0;
const SPARK_X: f32 = 38.0;
const SPARK_W: f32 = 120.0;
/// Power sparkline full scale (watts).
const POWER_SCALE: f32 = 80.0;

#[derive(Clone, Copy, Debug, Default)]
pub struct Sample {
    /// Seconds since the monitor started.
    pub t: f64,
    /// 0..1.
    pub cpu: Option<f32>,
    /// Used and total bytes.
    pub mem: Option<(u64, u64)>,
    /// 0..1.
    pub gpu: Option<f32>,
    pub gpu_w: Option<f32>,
    pub ac: Option<bool>,
    /// Percent.
    pub battery: Option<f32>,
    /// Watts drawn from the battery while discharging.
    pub battery_w: Option<f32>,
    /// Received, sent (bytes per second).
    pub net: Option<(f64, f64)>,
}

pub struct SysMon {
    history: Arc<Mutex<VecDeque<Sample>>>,
    active: Arc<AtomicBool>,
    start: Instant,
}

impl SysMon {
    /// Starts sampling. `vendor` is the PCI vendor id of the GPU in use.
    pub fn start(vendor: u32) -> Self {
        let history = Arc::new(Mutex::new(VecDeque::new()));
        let active = Arc::new(AtomicBool::new(true));
        let start = Instant::now();
        let (h, a) = (Arc::downgrade(&history), active.clone());
        let spawned = std::thread::Builder::new().name("sysmon".into()).spawn(move || {
            let mut r = Readers::new(vendor);
            loop {
                let Some(h) = h.upgrade() else { return };
                if a.load(Ordering::Relaxed) {
                    let s = r.sample(start.elapsed().as_secs_f64());
                    let mut q = h.lock().unwrap_or_else(|e| e.into_inner());
                    q.push_back(s);
                    while q.len() > HISTORY_S as usize + 4 {
                        q.pop_front();
                    }
                } else {
                    r.reset();
                }
                drop(h);
                std::thread::sleep(Duration::from_secs(1));
            }
        });
        if let Err(e) = spawned {
            log::warn!("system monitor thread: {e}");
        }
        Self { history, active, start }
    }

    /// Sampling runs only while the readout is on screen.
    pub fn set_active(&self, on: bool) {
        self.active.store(on, Ordering::Relaxed);
    }

    /// Overlay sprites in the lower right of a `w`×`h` screen.
    pub fn build(&self, cfg: &Config, atlas: &GlyphAtlas, w: f32, h: f32, out: &mut Vec<SpriteVertex>) {
        let now = self.start.elapsed().as_secs_f64();
        let hist: Vec<Sample> = self.history.lock().unwrap_or_else(|e| e.into_inner()).iter().copied().collect();
        draw(&hist, now, cfg, atlas, w, h, out);
    }
}

/// Screen rectangle (x0, y0, x1, y1) the readout occupies on a `w`×`h` screen.
pub fn rect(w: f32, h: f32) -> [f32; 4] {
    let ui = w.min(h) / 1080.0;
    let (right, bottom) = (w - MARGIN_R * ui, h - MARGIN_B * ui);
    [right - (WIDTH + 12.0) * ui, bottom - (ROWS.len() as f32 + 1.0) * PITCH * ui - 8.0 * ui, w, h]
}

fn draw(hist: &[Sample], now: f64, cfg: &Config, atlas: &GlyphAtlas, w: f32, h: f32, out: &mut Vec<SpriteVertex>) {
    let ui = w.min(h) / 1080.0;
    let face = &atlas.mono;
    let gold = cfg.color(&cfg.age.fresh);
    let alert = cfg.color(&cfg.events.write.color);
    let col = |c: [f32; 3], a: f32| [c[0], c[1], c[2], a];
    let (right, bottom) = (w - MARGIN_R * ui, h - MARGIN_B * ui);
    let left = right - WIDTH * ui;
    let (x0, x1) = (left + SPARK_X * ui, left + (SPARK_X + SPARK_W) * ui);
    let last = hist.last().copied().unwrap_or_default();

    // Header: 電脳 ("cyberbrain") and a thin rule.
    let head_y = bottom - ROWS.len() as f32 * PITCH * ui - 2.0 * ui;
    text_px(out, &atlas.cjk, "電脳", left, head_y, 11.0 * ui, col(gold, 0.4), w, h);
    text_px(out, face, "SYSTEM", left + 28.0 * ui, head_y, 9.0 * ui, col(gold, 0.35), w, h);
    line_px(out, [left + 80.0 * ui, head_y - 3.0 * ui], [right, head_y - 3.0 * ui], 1.0 * ui, col(gold, 0.18), w, h);

    for (i, label) in ROWS.iter().enumerate() {
        let y = bottom - (ROWS.len() - 1 - i) as f32 * PITCH * ui;
        let (value, level): (String, Option<f32>) = match i {
            0 => (last.cpu.map_or("--".into(), |c| format!("{:.0}%", c * 100.0)), last.cpu),
            1 => (
                last.mem.map_or("--".into(), |(u, t)| format!("{:.1} / {:.0} GB", u as f64 / GIB, t as f64 / GIB)),
                last.mem.map(|(u, t)| u as f32 / t.max(1) as f32),
            ),
            2 => (last.gpu.map_or("--".into(), |g| format!("{:.0}%", g * 100.0)), last.gpu),
            3 => (power_text(&last), None),
            _ => (last.net.map_or("--".into(), |(rx, tx)| format!("↓{}  ↑{}", rate(rx), rate(tx))), None),
        };
        let hot = level.is_some_and(|l| l > 0.9);
        text_px(out, face, label, left, y, 10.0 * ui, col(gold, 0.42), w, h);
        let vs = 11.0 * ui;
        text_px(out, face, &value, right - face.measure(&value, vs), y, vs, col(if hot { alert } else { gold }, 0.72), w, h);

        // Sparkline: newest sample at the right, scrolling left continuously.
        let (yb, yt) = (y + 1.0 * ui, y - 10.0 * ui);
        line_px(out, [x0, yb], [x1, yb], 1.0 * ui, col(gold, 0.1), w, h);
        let pts: Vec<(f32, f32, f64)> = hist
            .iter()
            .filter_map(|s| {
                let v = series(s, i)?;
                let age = now - s.t;
                (age <= HISTORY_S + 2.0).then(|| (x1 - (age / HISTORY_S) as f32 * (x1 - x0), yb - v.clamp(0.0, 1.0) * (yb - yt), s.t))
            })
            .collect();
        for pair in pts.windows(2) {
            let ((mut ax, mut ay, ta), (bx, by, tb)) = (pair[0], pair[1]);
            if tb - ta > 2.5 || bx <= x0 {
                continue;
            }
            if ax < x0 {
                ay += (by - ay) * (x0 - ax) / (bx - ax).max(1e-3);
                ax = x0;
            }
            let fade = smoothstep(x0, x0 + 16.0 * ui, ax);
            let c = if hot { alert } else { gold };
            quad_px(out, [[ax, yb], [bx, yb], [bx, by], [ax, ay]], col(c, 0.07 * fade), w, h);
            line_px(out, [ax, ay], [bx, by], 1.1 * ui, col(c, 0.5 * fade), w, h);
        }
    }
}

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Row `i`'s value for the sparkline, 0..1.
fn series(s: &Sample, i: usize) -> Option<f32> {
    match i {
        0 => s.cpu,
        1 => s.mem.map(|(u, t)| u as f32 / t.max(1) as f32),
        2 => s.gpu,
        3 => s.battery_w.or(s.gpu_w).map(|w| w / POWER_SCALE),
        _ => s.net.map(|(rx, tx)| {
            // Log scale, 1 KB/s .. 100 MB/s.
            ((1.0 + (rx + tx) / 1e3).log10() / 5.0) as f32
        }),
    }
}

fn power_text(s: &Sample) -> String {
    match (s.ac, s.battery, s.battery_w, s.gpu_w) {
        (Some(false), b, Some(w), _) => format!("{w:.1} W  BAT {:.0}%", b.unwrap_or(0.0)),
        (_, _, _, Some(g)) => format!("{}GPU {g:.0} W", if s.ac == Some(true) { "AC  " } else { "" }),
        (Some(true), Some(b), _, _) => format!("AC  BAT {b:.0}%"),
        (Some(true), None, _, _) => "AC".into(),
        _ => "--".into(),
    }
}

fn rate(b: f64) -> String {
    if b < 1e3 {
        format!("{b:.0} B/s")
    } else if b < 1e6 {
        format!("{:.0} KB/s", b / 1e3)
    } else if b < 1e9 {
        format!("{:.1} MB/s", b / 1e6)
    } else {
        format!("{:.1} GB/s", b / 1e9)
    }
}

// ---------------------------------------------------------------------------
// Readers
// ---------------------------------------------------------------------------

struct Readers {
    cpu_prev: Option<(u64, u64)>,
    net_prev: Option<(f64, u64, u64)>,
    gpu: Gpu,
}

enum Gpu {
    Nvml(nvml::Nvml),
    /// Intel: busy = 1 - (time in the RC6 sleep state) / (time elapsed).
    IntelRc6 {
        path: PathBuf,
        prev: Option<(f64, u64)>,
    },
    Amd {
        busy: PathBuf,
        power: Option<PathBuf>,
    },
    None,
}

impl Readers {
    fn new(vendor: u32) -> Self {
        let gpu = match vendor {
            0x10de => nvml::Nvml::load().map(Gpu::Nvml),
            0x8086 => drm_card(0x8086).and_then(|card| {
                ["gt/gt0/rc6_residency_ms", "power/rc6_residency_ms"]
                    .iter()
                    .map(|p| card.join(p))
                    .find(|p| p.exists())
                    .map(|path| Gpu::IntelRc6 { path, prev: None })
            }),
            0x1002 => drm_card(0x1002).map(|card| {
                let hwmon = std::fs::read_dir(card.join("device/hwmon")).ok().and_then(|mut d| d.next()).and_then(|e| e.ok());
                Gpu::Amd {
                    busy: card.join("device/gpu_busy_percent"),
                    power: hwmon.map(|e| e.path().join("power1_average")).filter(|p| p.exists()),
                }
            }),
            _ => None,
        }
        .unwrap_or(Gpu::None);
        Self { cpu_prev: None, net_prev: None, gpu }
    }

    /// Forgets the previous counters (after a pause, deltas start over).
    fn reset(&mut self) {
        self.cpu_prev = None;
        self.net_prev = None;
        if let Gpu::IntelRc6 { prev, .. } = &mut self.gpu {
            *prev = None;
        }
    }

    fn sample(&mut self, t: f64) -> Sample {
        let mut s = Sample { t, ..Default::default() };
        if let Some((total, idle)) = read_cpu() {
            if let Some((t0, i0)) = self.cpu_prev
                && total > t0
            {
                s.cpu = Some(1.0 - (idle.saturating_sub(i0)) as f32 / (total - t0) as f32);
            }
            self.cpu_prev = Some((total, idle));
        }
        s.mem = read_mem();
        if let Some((rx, tx)) = read_net() {
            if let Some((t0, rx0, tx0)) = self.net_prev
                && t > t0
            {
                s.net = Some((rx.saturating_sub(rx0) as f64 / (t - t0), tx.saturating_sub(tx0) as f64 / (t - t0)));
            }
            self.net_prev = Some((t, rx, tx));
        }
        match &mut self.gpu {
            Gpu::Nvml(n) => (s.gpu, s.gpu_w) = n.read(),
            Gpu::IntelRc6 { path, prev } => {
                if let Some(rc6) = read_u64(path) {
                    if let Some((t0, r0)) = *prev
                        && t > t0
                    {
                        let asleep = rc6.saturating_sub(r0) as f64 / ((t - t0) * 1e3);
                        s.gpu = Some((1.0 - asleep).clamp(0.0, 1.0) as f32);
                    }
                    *prev = Some((t, rc6));
                }
            }
            Gpu::Amd { busy, power } => {
                s.gpu = read_u64(busy).map(|b| b as f32 / 100.0);
                s.gpu_w = power.as_ref().and_then(|p| read_u64(p)).map(|uw| uw as f32 / 1e6);
            }
            Gpu::None => {}
        }
        (s.ac, s.battery, s.battery_w) = read_power();
        s
    }
}

fn read_u64(p: &Path) -> Option<u64> {
    std::fs::read_to_string(p).ok()?.trim().parse().ok()
}

fn read_str(p: &Path) -> String {
    std::fs::read_to_string(p).map(|s| s.trim().to_string()).unwrap_or_default()
}

/// The first /sys/class/drm/cardN of a PCI vendor.
fn drm_card(vendor: u32) -> Option<PathBuf> {
    let mut cards: Vec<PathBuf> = std::fs::read_dir("/sys/class/drm")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("card") && !n.contains('-')))
        .collect();
    cards.sort();
    cards.into_iter().find(|c| u32::from_str_radix(read_str(&c.join("device/vendor")).trim_start_matches("0x"), 16) == Ok(vendor))
}

/// (total, idle) jiffies from /proc/stat.
fn read_cpu() -> Option<(u64, u64)> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    parse_cpu(stat.lines().next()?)
}

fn parse_cpu(line: &str) -> Option<(u64, u64)> {
    let v: Vec<u64> = line.strip_prefix("cpu ")?.split_whitespace().take(8).filter_map(|x| x.parse().ok()).collect();
    (v.len() >= 5).then(|| (v.iter().sum(), v[3] + v[4]))
}

/// (used, total) bytes from /proc/meminfo.
fn read_mem() -> Option<(u64, u64)> {
    parse_mem(&std::fs::read_to_string("/proc/meminfo").ok()?)
}

fn parse_mem(s: &str) -> Option<(u64, u64)> {
    let field = |name: &str| -> Option<u64> {
        s.lines().find(|l| l.starts_with(name))?.split_whitespace().nth(1)?.parse::<u64>().ok().map(|kb| kb * 1024)
    };
    let (total, avail) = (field("MemTotal:")?, field("MemAvailable:")?);
    Some((total.saturating_sub(avail), total))
}

/// Total (received, sent) bytes over physical interfaces, from /proc/net/dev.
fn read_net() -> Option<(u64, u64)> {
    Some(parse_net(&std::fs::read_to_string("/proc/net/dev").ok()?))
}

fn parse_net(s: &str) -> (u64, u64) {
    const VIRTUAL: [&str; 9] = ["lo", "docker", "veth", "br-", "virbr", "vnet", "tun", "tap", "wg"];
    let (mut rx, mut tx) = (0, 0);
    for line in s.lines().skip(2) {
        let Some((name, rest)) = line.split_once(':') else { continue };
        if VIRTUAL.iter().any(|v| name.trim().starts_with(v)) {
            continue;
        }
        let f: Vec<u64> = rest.split_whitespace().filter_map(|x| x.parse().ok()).collect();
        if f.len() >= 9 {
            rx += f[0];
            tx += f[8];
        }
    }
    (rx, tx)
}

/// (on mains, battery percent, watts drawn from the battery).
fn read_power() -> (Option<bool>, Option<f32>, Option<f32>) {
    let Ok(rd) = std::fs::read_dir("/sys/class/power_supply") else { return (None, None, None) };
    let (mut ac, mut pct, mut watts) = (None, None, None);
    for e in rd.flatten() {
        let p = e.path();
        match read_str(&p.join("type")).as_str() {
            // Peripherals (mice, headsets) report scope "Device".
            "Battery" if read_str(&p.join("scope")) != "Device" => {
                pct = read_u64(&p.join("capacity")).map(|c| c as f32);
                if read_str(&p.join("status")) == "Discharging" {
                    watts = read_u64(&p.join("power_now")).map(|uw| uw as f32 / 1e6).or_else(|| {
                        let (i, v) = (read_u64(&p.join("current_now"))?, read_u64(&p.join("voltage_now"))?);
                        Some((i as f64 * v as f64 / 1e12) as f32)
                    });
                }
            }
            "Mains" | "USB" => {
                let online = read_u64(&p.join("online")) == Some(1);
                ac = Some(ac.unwrap_or(false) || online);
            }
            _ => {}
        }
    }
    (ac, pct, watts)
}

/// NVIDIA's management library (part of the driver), loaded if present.
mod nvml {
    use std::ffi::c_void;

    type Device = *mut c_void;

    #[repr(C)]
    struct Utilization {
        gpu: u32,
        memory: u32,
    }

    pub struct Nvml {
        _lib: libloading::Library,
        dev: Device,
        util: unsafe extern "C" fn(Device, *mut Utilization) -> u32,
        power: unsafe extern "C" fn(Device, *mut u32) -> u32,
    }

    impl Nvml {
        pub fn load() -> Option<Self> {
            // SAFETY: NVML's documented C entry points, called with the
            // signatures from nvml.h; the library stays loaded while `Nvml` lives.
            unsafe {
                let lib = libloading::Library::new("libnvidia-ml.so.1").ok()?;
                let init = *lib.get::<unsafe extern "C" fn() -> u32>(b"nvmlInit_v2\0").ok()?;
                if init() != 0 {
                    return None;
                }
                let by_index = *lib.get::<unsafe extern "C" fn(u32, *mut Device) -> u32>(b"nvmlDeviceGetHandleByIndex_v2\0").ok()?;
                let mut dev: Device = std::ptr::null_mut();
                if by_index(0, &mut dev) != 0 {
                    return None;
                }
                let util = *lib.get(b"nvmlDeviceGetUtilizationRates\0").ok()?;
                let power = *lib.get(b"nvmlDeviceGetPowerUsage\0").ok()?;
                Some(Self { _lib: lib, dev, util, power })
            }
        }

        /// (busy 0..1, watts).
        pub fn read(&self) -> (Option<f32>, Option<f32>) {
            let mut u = Utilization { gpu: 0, memory: 0 };
            let mut mw = 0u32;
            // SAFETY: see `load`; the out-pointers are valid locals.
            let (ru, rp) = unsafe { ((self.util)(self.dev, &mut u), (self.power)(self.dev, &mut mw)) };
            let _ = u.memory;
            ((ru == 0).then_some(u.gpu as f32 / 100.0), (rp == 0).then_some(mw as f32 / 1000.0))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_proc_files() {
        assert_eq!(parse_cpu("cpu  100 0 50 800 50 0 0 0 0 0"), Some((1000, 850)));
        let mem = "MemTotal:       32000000 kB\nMemFree:  1000 kB\nMemAvailable:   20000000 kB\n";
        assert_eq!(parse_mem(mem), Some((12000000 * 1024, 32000000 * 1024)));
        let net = "Inter-|   Receive |  Transmit\n face |bytes packets|bytes\n    lo: 500 5 0 0 0 0 0 0 500 5 0 0 0 0 0 0\n  wlp0s20f3: 1000 10 0 0 0 0 0 0 200 2 0 0 0 0 0 0\ndocker0: 7 1 0 0 0 0 0 0 7 1 0 0 0 0 0 0\n";
        assert_eq!(parse_net(net), (1000, 200));
    }

    #[test]
    fn texts() {
        assert_eq!(rate(1500.0), "2 KB/s");
        assert_eq!(rate(2.5e6), "2.5 MB/s");
        let s = Sample { ac: Some(false), battery: Some(76.0), battery_w: Some(14.24), ..Default::default() };
        assert_eq!(power_text(&s), "14.2 W  BAT 76%");
        let s = Sample { ac: Some(true), battery: Some(100.0), gpu_w: Some(22.4), ..Default::default() };
        assert_eq!(power_text(&s), "AC  GPU 22 W");
    }
}
