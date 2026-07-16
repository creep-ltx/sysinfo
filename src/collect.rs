//! System data collection: static facts gathered once, cheap /proc//sys reads
//! sampled continuously, and slow external commands cached on individual timers.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn run_cmd(argv: &[&str]) -> String {
    if argv.is_empty() {
        return String::new();
    }
    Command::new(argv[0])
        .args(&argv[1..])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default()
}

fn read_trim(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).unwrap_or_default().trim().to_string()
}

fn read_u64(path: impl AsRef<Path>) -> u64 {
    read_trim(path).parse().unwrap_or(0)
}

pub fn which(bin: &str) -> bool {
    std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).any(|d| d.join(bin).is_file()))
        .unwrap_or(false)
}

fn euid() -> u32 {
    // effective uid is the third field of the Uid: line
    fs::read_to_string("/proc/self/status")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with("Uid:"))
        .and_then(|l| l.split_whitespace().nth(2))
        .and_then(|s| s.parse().ok())
        .unwrap_or(u32::MAX)
}

fn user_writable(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let me = euid();
    if me == 0 {
        return true;
    }
    path.metadata()
        .map(|m| m.uid() == me && m.mode() & 0o200 != 0)
        .unwrap_or(false)
}

// ---------------------------------------------------------------------------
// static facts: collected once at startup

pub struct GpuInfo {
    pub driver: String,
    pub name: String,
    pub hwmon_id: Option<String>, // "hwmonN", matches HwmonChip::id
    pub dev_path: PathBuf,
}

pub struct StaticInfo {
    pub hostname: String,
    pub os_name: String,
    pub kernel: String,
    pub arch: String,
    pub cpu_model: String,
    pub wifi_standard: String,
    pub wifi_bands: String,
    pub gpus: Vec<GpuInfo>,
}

impl StaticInfo {
    pub fn collect() -> Self {
        let (wifi_standard, wifi_bands) = wifi_capabilities();
        Self {
            hostname: read_trim("/proc/sys/kernel/hostname"),
            os_name: os_pretty_name(),
            kernel: read_trim("/proc/sys/kernel/osrelease"),
            arch: std::env::consts::ARCH.to_string(),
            cpu_model: cpuinfo_field("model name"),
            wifi_standard,
            wifi_bands,
            gpus: detect_gpus(),
        }
    }
}

fn os_pretty_name() -> String {
    fs::read_to_string("/etc/os-release")
        .unwrap_or_default()
        .lines()
        .find_map(|l| l.strip_prefix("PRETTY_NAME="))
        .map(|v| v.trim_matches('"').to_string())
        .unwrap_or_else(|| "Linux".to_string())
}

fn cpuinfo_field(field: &str) -> String {
    fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .find(|l| l.starts_with(field))
        .and_then(|l| l.split(':').nth(1))
        .map(|v| v.trim().to_string())
        .unwrap_or_else(|| "Unknown".to_string())
}

fn detect_gpus() -> Vec<GpuInfo> {
    let mut gpus = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/drm") else {
        return gpus;
    };
    let lspci = run_cmd(&["lspci", "-mm"]); // once, for human-readable names
    let mut cards: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.strip_prefix("card").is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit())))
        .collect();
    cards.sort();
    for card in cards {
        let dev = PathBuf::from(format!("/sys/class/drm/{card}/device"));
        let driver = fs::read_link(dev.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .unwrap_or_else(|| "unknown".to_string());
        let pci_addr = fs::canonicalize(&dev)
            .ok()
            .and_then(|p| p.file_name().map(|f| f.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let name = lspci_name(&lspci, &pci_addr).unwrap_or_else(|| format!("GPU [{driver}]"));
        let hwmon_id = fs::read_dir(dev.join("hwmon")).ok().and_then(|mut d| {
            d.next()
                .and_then(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().into_owned())
        });
        gpus.push(GpuInfo { driver, name, hwmon_id, dev_path: dev });
    }
    gpus
}

fn lspci_name(lspci: &str, pci_addr: &str) -> Option<String> {
    let short = pci_addr.strip_prefix("0000:").unwrap_or(pci_addr);
    if short.is_empty() {
        return None;
    }
    let line = lspci.lines().find(|l| l.starts_with(short))?;
    let fields: Vec<&str> = line.split('"').collect();
    if fields.len() < 6 {
        return None;
    }
    let vendor = fields[3];
    // "Advanced Micro Devices, Inc. [AMD/ATI]" -> "AMD/ATI"
    let vendor = vendor
        .rsplit_once('[')
        .and_then(|(_, r)| r.strip_suffix(']'))
        .unwrap_or(vendor);
    Some(format!("{} {}", vendor, fields[5]))
}

fn wifi_capabilities() -> (String, String) {
    let output = run_cmd(&["iw", "phy"]);
    if output.is_empty() {
        return ("N/A".to_string(), "N/A".to_string());
    }
    let (mut ht, mut vht, mut he, mut eht) = (false, false, false, false);
    let (mut g24, mut g5, mut g6) = (false, false, false);
    for line in output.lines() {
        if line.contains("EHT Capabilities") {
            eht = true;
        } else if line.contains("HE Capabilities") || line.contains("HE MAC Capabilities") {
            he = true;
        } else if line.contains("VHT Capabilities") {
            vht = true;
        } else if line.contains("HT Capabilities") {
            ht = true;
        }
        if let Some(freq) = line
            .find("MHz")
            .and_then(|pos| line[..pos].split_whitespace().last())
            .and_then(|w| w.parse::<f64>().ok())
        {
            if (2400.0..2500.0).contains(&freq) {
                g24 = true;
            } else if (5000.0..5900.0).contains(&freq) {
                g5 = true;
            } else if (5900.0..7200.0).contains(&freq) {
                g6 = true;
            }
        }
    }
    let standard = if eht {
        "Wi-Fi 7 (802.11be)"
    } else if he && g6 {
        "Wi-Fi 6E (802.11ax)"
    } else if he {
        "Wi-Fi 6 (802.11ax)"
    } else if vht {
        "Wi-Fi 5 (802.11ac)"
    } else if ht {
        "Wi-Fi 4 (802.11n)"
    } else {
        "Legacy (802.11a/b/g)"
    };
    let mut bands = Vec::new();
    if g24 {
        bands.push("2.4 GHz");
    }
    if g5 {
        bands.push("5 GHz");
    }
    if g6 {
        bands.push("6 GHz");
    }
    let bands = if bands.is_empty() { "None".to_string() } else { bands.join(", ") };
    (standard.to_string(), bands)
}

// ---------------------------------------------------------------------------
// hwmon: generic sensor enumeration, replaces parsing `sensors` output

pub const CPU_CHIPS: &[&str] = &["k10temp", "zenpower", "coretemp", "cpu_thermal"];

#[derive(Default, Clone)]
pub struct HwmonChip {
    pub id: String,   // "hwmonN"
    pub name: String, // driver name, e.g. "k10temp", "amdgpu", "nvme"
    pub temps: Vec<(String, f64)>,  // °C
    pub fans: Vec<(String, u64)>,   // RPM
    pub volts: Vec<(String, f64)>,  // V
    pub freqs: Vec<(String, f64)>,  // MHz
    pub powers: Vec<(String, f64)>, // W
}

impl HwmonChip {
    pub fn is_empty(&self) -> bool {
        self.temps.is_empty()
            && self.fans.is_empty()
            && self.volts.is_empty()
            && self.freqs.is_empty()
            && self.powers.is_empty()
    }
}

fn sensor_sort_key(file: &str) -> (String, u32) {
    let stem = file.split('_').next().unwrap_or(file);
    let digits = stem.find(|c: char| c.is_ascii_digit()).unwrap_or(stem.len());
    (stem[..digits].to_string(), stem[digits..].parse().unwrap_or(0))
}

fn read_hwmon_dir(dir: &Path, id: &str) -> HwmonChip {
    let mut chip = HwmonChip { id: id.to_string(), name: read_trim(dir.join("name")), ..Default::default() };
    let Ok(entries) = fs::read_dir(dir) else {
        return chip;
    };
    let mut files: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|f| f.ends_with("_input") || f.ends_with("_average"))
        .collect();
    files.sort_by_key(|f| sensor_sort_key(f));
    for file in &files {
        let sensor = file.trim_end_matches("_input").trim_end_matches("_average");
        // powerN often only exposes _average; skip _average for everything else
        if file.ends_with("_average") && !sensor.starts_with("power") {
            continue;
        }
        if sensor.starts_with("power") && chip.powers.iter().any(|(l, _)| l == sensor) {
            continue; // already got this one via _input
        }
        let Ok(raw) = read_trim(dir.join(file)).parse::<f64>() else {
            continue;
        };
        let label_file = dir.join(format!("{sensor}_label"));
        let label = if label_file.exists() { read_trim(label_file) } else { sensor.to_string() };
        if sensor.starts_with("temp") {
            chip.temps.push((label, raw / 1000.0));
        } else if sensor.starts_with("fan") {
            chip.fans.push((label, raw as u64));
        } else if sensor.starts_with("in") {
            chip.volts.push((label, raw / 1000.0));
        } else if sensor.starts_with("freq") {
            chip.freqs.push((label, raw / 1_000_000.0));
        } else if sensor.starts_with("power") {
            chip.powers.push((sensor.to_string(), raw / 1_000_000.0));
        }
    }
    chip
}

fn read_all_hwmon() -> Vec<HwmonChip> {
    let mut chips = Vec::new();
    let Ok(entries) = fs::read_dir("/sys/class/hwmon") else {
        return chips;
    };
    let mut ids: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    ids.sort_by_key(|f| sensor_sort_key(f));
    for id in ids {
        let chip = read_hwmon_dir(&PathBuf::from(format!("/sys/class/hwmon/{id}")), &id);
        if !chip.is_empty() {
            chips.push(chip);
        }
    }
    chips
}

// ---------------------------------------------------------------------------
// continuously sampled + cached dynamic data

#[derive(Clone, Copy, Default)]
struct CpuTimes {
    idle: u64,
    total: u64,
}

#[derive(Clone, Copy, Default)]
struct NetBytes {
    rx: u64,
    tx: u64,
}

pub struct Sampler {
    prev_global: CpuTimes,
    prev_cores: Vec<CpuTimes>,
    prev_net: HashMap<String, NetBytes>,
    last_sample: Instant,
    pub cpu_load: f64,
    pub core_loads: Vec<f64>,
    pub net_speeds: HashMap<String, (f64, f64)>, // KB/s (down, up)
    hwmon_at: Option<Instant>,
    hwmon: Vec<HwmonChip>,
    disks_at: Option<Instant>,
    disks: String,
    ips_at: Option<Instant>,
    ips: HashMap<String, String>,
}

const HWMON_TTL: Duration = Duration::from_secs(2);
const DISKS_TTL: Duration = Duration::from_secs(5);
const IPS_TTL: Duration = Duration::from_secs(10);

fn expired(at: Option<Instant>, ttl: Duration) -> bool {
    at.is_none_or(|t| t.elapsed() >= ttl)
}

impl Sampler {
    pub fn new() -> Self {
        let mut s = Self {
            prev_global: CpuTimes::default(),
            prev_cores: Vec::new(),
            prev_net: HashMap::new(),
            last_sample: Instant::now(),
            cpu_load: 0.0,
            core_loads: Vec::new(),
            net_speeds: HashMap::new(),
            hwmon_at: None,
            hwmon: Vec::new(),
            disks_at: None,
            disks: String::new(),
            ips_at: None,
            ips: HashMap::new(),
        };
        s.sample(); // prime the deltas
        s
    }

    /// Cheap /proc reads; call once per frame.
    pub fn sample(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_sample).as_secs_f64();
        if elapsed < 0.1 && self.prev_global.total > 0 {
            return;
        }

        let (global, cores) = cpu_times();
        if self.prev_global.total > 0 {
            self.cpu_load = load_between(self.prev_global, global);
        }
        self.core_loads = cores
            .iter()
            .enumerate()
            .map(|(i, &c)| self.prev_cores.get(i).map_or(0.0, |&p| load_between(p, c)))
            .collect();
        self.prev_global = global;
        self.prev_cores = cores;

        let net = net_bytes();
        for (iface, cur) in &net {
            let speed = self.prev_net.get(iface).map_or((0.0, 0.0), |prev| {
                (
                    (cur.rx.saturating_sub(prev.rx) as f64 / 1024.0) / elapsed.max(0.1),
                    (cur.tx.saturating_sub(prev.tx) as f64 / 1024.0) / elapsed.max(0.1),
                )
            });
            self.net_speeds.insert(iface.clone(), speed);
        }
        self.prev_net = net;
        self.last_sample = now;
    }

    pub fn hwmon(&mut self) -> &[HwmonChip] {
        if expired(self.hwmon_at, HWMON_TTL) {
            self.hwmon = read_all_hwmon();
            self.hwmon_at = Some(Instant::now());
        }
        &self.hwmon
    }

    pub fn hwmon_by_id(&mut self, id: &str) -> Option<HwmonChip> {
        self.hwmon(); // ensure fresh
        self.hwmon.iter().find(|c| c.id == id).cloned()
    }

    pub fn cpu_chip(&mut self) -> Option<HwmonChip> {
        self.hwmon();
        self.hwmon.iter().find(|c| CPU_CHIPS.contains(&c.name.as_str())).cloned()
    }

    pub fn disks(&mut self) -> &str {
        if expired(self.disks_at, DISKS_TTL) {
            self.disks = run_cmd(&["lsblk", "-o", "NAME,SIZE,TYPE,MOUNTPOINTS,FSUSED,FSUSE%"]);
            self.disks_at = Some(Instant::now());
        }
        &self.disks
    }

    pub fn ips(&mut self) -> &HashMap<String, String> {
        if expired(self.ips_at, IPS_TTL) {
            self.ips = local_ips();
            self.ips_at = Some(Instant::now());
        }
        &self.ips
    }
}

fn cpu_times() -> (CpuTimes, Vec<CpuTimes>) {
    let mut global = CpuTimes::default();
    let mut cores = Vec::new();
    for line in fs::read_to_string("/proc/stat").unwrap_or_default().lines() {
        if !line.starts_with("cpu") {
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() < 5 {
            continue;
        }
        let field = |i: usize| parts.get(i).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
        let idle = field(4) + field(5); // idle + iowait
        let total = field(1) + field(2) + field(3) + idle + field(6) + field(7) + field(8);
        let times = CpuTimes { idle, total };
        if parts[0] == "cpu" {
            global = times;
        } else {
            cores.push(times);
        }
    }
    (global, cores)
}

fn load_between(prev: CpuTimes, curr: CpuTimes) -> f64 {
    let total = curr.total.saturating_sub(prev.total);
    if total == 0 {
        return 0.0;
    }
    let active = total.saturating_sub(curr.idle.saturating_sub(prev.idle));
    (active as f64 / total as f64) * 100.0
}

fn net_bytes() -> HashMap<String, NetBytes> {
    let mut map = HashMap::new();
    for line in fs::read_to_string("/proc/net/dev").unwrap_or_default().lines() {
        if let Some((iface, stats)) = line.trim().split_once(':') {
            let parts: Vec<&str> = stats.split_whitespace().collect();
            if parts.len() >= 9 {
                map.insert(
                    iface.trim().to_string(),
                    NetBytes {
                        rx: parts[0].parse().unwrap_or(0),
                        tx: parts[8].parse().unwrap_or(0),
                    },
                );
            }
        }
    }
    map
}

fn local_ips() -> HashMap<String, String> {
    // one `ip` invocation covers every interface
    let output = run_cmd(&["ip", "-4", "-brief", "addr", "show"]);
    let mut map = HashMap::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 {
            map.insert(parts[0].to_string(), parts[2].to_string());
        }
    }
    map
}

// ---------------------------------------------------------------------------
// one-off readers used by the UI

pub fn uptime() -> String {
    let secs = read_trim("/proc/uptime")
        .split_whitespace()
        .next()
        .and_then(|s| s.parse::<f64>().ok())
        .unwrap_or(0.0) as u64;
    let (d, h, m) = (secs / 86400, (secs % 86400) / 3600, (secs % 3600) / 60);
    match (d, h) {
        (0, 0) => format!("{m}m"),
        (0, _) => format!("{h}h {m}m"),
        _ => format!("{d}d {h}h {m}m"),
    }
}

pub fn loadavg() -> (String, String) {
    let content = read_trim("/proc/loadavg");
    let parts: Vec<&str> = content.split_whitespace().collect();
    let load = if parts.len() >= 3 {
        format!("{}  {}  {}", parts[0], parts[1], parts[2])
    } else {
        "Unknown".to_string()
    };
    let sched = parts.get(3).unwrap_or(&"N/A").to_string();
    (load, sched)
}

pub fn process_count() -> usize {
    fs::read_dir("/proc")
        .map(|entries| {
            entries
                .flatten()
                .filter(|e| e.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit()))
                .count()
        })
        .unwrap_or(0)
}

pub fn cpu_governor() -> String {
    let g = read_trim("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor");
    if g.is_empty() { "Unknown".to_string() } else { g }
}

pub fn cpu_mhz() -> String {
    let freqs: Vec<f64> = fs::read_to_string("/proc/cpuinfo")
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with("cpu MHz"))
        .filter_map(|l| l.split(':').nth(1)?.trim().parse().ok())
        .collect();
    if freqs.is_empty() {
        return "Unknown".to_string();
    }
    let avg = freqs.iter().sum::<f64>() / freqs.len() as f64;
    let max = freqs.iter().cloned().fold(0.0, f64::max);
    format!("{avg:.0} MHz avg / {max:.0} MHz peak")
}

pub fn ram_usage() -> (u64, u64) {
    let mut total = 0;
    let mut available = 0;
    for line in fs::read_to_string("/proc/meminfo").unwrap_or_default().lines() {
        let kb = || line.split_whitespace().nth(1).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) * 1024;
        if line.starts_with("MemTotal:") {
            total = kb();
        } else if line.starts_with("MemAvailable:") {
            available = kb();
        }
    }
    (total, total.saturating_sub(available))
}

pub fn zram_info() -> String {
    let Ok(entries) = fs::read_dir("/sys/block") else {
        return "ZRAM: unavailable".to_string();
    };
    let mut devs: Vec<String> = entries
        .flatten()
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("zram"))
        .collect();
    devs.sort();
    if devs.is_empty() {
        return "No ZRAM devices configured".to_string();
    }
    let mut out = Vec::new();
    for dev in devs {
        let base = PathBuf::from(format!("/sys/block/{dev}"));
        let disksize = read_u64(base.join("disksize"));
        let orig = read_u64(base.join("orig_data_size"));
        let compr = read_u64(base.join("compr_data_size"));
        let algo = read_trim(base.join("comp_algorithm"));
        let active = algo
            .split_whitespace()
            .find(|s| s.starts_with('[') && s.ends_with(']'))
            .map(|s| s.trim_matches(['[', ']']))
            .unwrap_or("unknown");
        let ratio = if compr > 0 { orig as f64 / compr as f64 } else { 1.0 };
        out.push(format!(
            "{}: {:.2} GB swap, {} | data {:.1} MB -> {:.1} MB in RAM ({:.2}:1, {:.1} MB saved)",
            dev,
            disksize as f64 / 1024.0 / 1024.0 / 1024.0,
            active,
            orig as f64 / 1024.0 / 1024.0,
            compr as f64 / 1024.0 / 1024.0,
            ratio,
            (orig.saturating_sub(compr)) as f64 / 1024.0 / 1024.0,
        ));
    }
    out.join("\n")
}

pub fn amd_vram(dev_path: &Path) -> Option<(u64, u64)> {
    let total = read_u64(dev_path.join("mem_info_vram_total"));
    if total == 0 {
        return None;
    }
    Some((total, read_u64(dev_path.join("mem_info_vram_used"))))
}

// ---------------------------------------------------------------------------
// background package update check (read-only commands, never elevates)

#[derive(Default)]
pub struct UpdatesInfo {
    pub lines: Vec<String>,
    pub checking: bool,
    pub last_checked: Option<Instant>,
}

pub fn check_updates(state: &Arc<Mutex<UpdatesInfo>>) {
    if let Ok(mut lock) = state.lock() {
        if lock.checking {
            return;
        }
        lock.checking = true;
    }

    let count = |out: String| out.lines().filter(|l| !l.trim().is_empty()).count();
    let mut lines = Vec::new();

    if which("checkupdates") {
        lines.push(format!("Pacman (Official): {} updates", count(run_cmd(&["checkupdates"]))));
    }
    if let Some(helper) = ["paru", "yay", "pikaur"].iter().find(|h| which(h)) {
        lines.push(format!("AUR ({helper}):        {} updates", count(run_cmd(&[helper, "-Qua"]))));
    }
    if which("flatpak") {
        lines.push(format!(
            "Flatpak:           {} updates",
            count(run_cmd(&["flatpak", "remote-ls", "--updates"]))
        ));
    }
    if which("npm") {
        let root = run_cmd(&["npm", "root", "-g"]).trim().to_string();
        if !root.is_empty() && user_writable(Path::new(&root)) {
            lines.push(format!(
                "NPM (Global):      {} updates",
                count(run_cmd(&["npm", "outdated", "-g", "--parseable"]))
            ));
        } else {
            lines.push("NPM (Global):      managed by pacman".to_string());
        }
    }
    if lines.is_empty() {
        lines.push("No supported package managers found".to_string());
    }

    if let Ok(mut lock) = state.lock() {
        lock.lines = lines;
        lock.checking = false;
        lock.last_checked = Some(Instant::now());
    }
}
