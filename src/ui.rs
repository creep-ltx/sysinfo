//! Tab content rendering: styled ratatui Text built from collected data.

use std::sync::{Arc, Mutex};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span, Text};

use crate::collect::{self, HwmonChip, Sampler, StaticInfo, UpdatesInfo, CPU_CHIPS};

pub const TABS: &[&str] = &[
    "[1] System & Load",
    "[2] CPU Cores",
    "[3] Memory & Disk",
    "[4] GPUs",
    "[5] Network",
    "[6] All Sensors",
];

// palette
pub const ACCENT: Color = Color::Rgb(122, 190, 222); // soft cyan: labels, borders
const TITLE: Color = Color::Rgb(247, 199, 103); // amber: section headers
const VALUE: Color = Color::Rgb(226, 226, 226);
const MUTED: Color = Color::Rgb(120, 125, 140);
const GOOD: Color = Color::Rgb(105, 222, 133);
const WARN: Color = Color::Rgb(247, 199, 103);
const POWER: Color = Color::Rgb(207, 142, 245); // soft magenta: wattage

const SPARK_CHARS: [char; 8] = ['▁', '▂', '▃', '▄', '▅', '▆', '▇', '█'];
const SPINNER: [char; 8] = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠇'];

fn lerp(a: (u8, u8, u8), b: (u8, u8, u8), t: f64) -> (u8, u8, u8) {
    let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    (mix(a.0, b.0), mix(a.1, b.1), mix(a.2, b.2))
}

/// green -> amber -> red over 0.0..=1.0, btop-style
fn ramp(t: f64) -> Color {
    let t = t.clamp(0.0, 1.0);
    let (r, g, b) = if t < 0.5 {
        lerp((105, 222, 133), (247, 199, 103), t * 2.0)
    } else {
        lerp((247, 199, 103), (245, 97, 97), (t - 0.5) * 2.0)
    };
    Color::Rgb(r, g, b)
}

fn header(title: &str) -> Line<'static> {
    let pad = 46usize.saturating_sub(title.chars().count() + 4);
    Line::from(Span::styled(
        format!("── {title} {}", "─".repeat(pad)),
        Style::default().fg(TITLE).add_modifier(Modifier::BOLD),
    ))
}

fn label(text: &str) -> Span<'static> {
    Span::styled(format!("{text:<18}"), Style::default().fg(ACCENT))
}

fn kv(key: &str, value: impl Into<String>) -> Line<'static> {
    Line::from(vec![label(key), Span::styled(value.into(), Style::default().fg(VALUE))])
}

fn kv_spans(key: &str, mut spans: Vec<Span<'static>>) -> Line<'static> {
    let mut all = vec![label(key)];
    all.append(&mut spans);
    Line::from(all)
}

fn blank() -> Line<'static> {
    Line::from("")
}

fn dim(text: impl Into<String>) -> Line<'static> {
    Line::from(Span::styled(text.into(), Style::default().fg(MUTED)))
}

/// `[██████░░░░] 42.0%` with a green->red gradient across the bar
fn bar_spans(pct: f64, width: usize) -> Vec<Span<'static>> {
    let filled = ((pct / 100.0) * width as f64).round().clamp(0.0, width as f64) as usize;
    let mut spans = vec![Span::styled("[", Style::default().fg(MUTED))];
    for i in 0..width {
        if i < filled {
            spans.push(Span::styled("█", Style::default().fg(ramp(i as f64 / width as f64))));
        } else {
            spans.push(Span::styled("░", Style::default().fg(Color::Rgb(60, 63, 75))));
        }
    }
    spans.push(Span::styled("] ", Style::default().fg(MUTED)));
    spans.push(Span::styled(format!("{pct:5.1}%"), Style::default().fg(ramp(pct / 100.0))));
    spans
}

/// sparkline over the last `width` samples; `fixed_max` pins the scale
/// (e.g. 100 for percentages), otherwise it auto-scales to the window peak
fn spark_spans(history: &[u64], width: usize, fixed_max: Option<u64>) -> Vec<Span<'static>> {
    let window: Vec<u64> = history.iter().rev().take(width).rev().copied().collect();
    let max = fixed_max.unwrap_or_else(|| window.iter().copied().max().unwrap_or(1).max(1));
    let mut spans = Vec::with_capacity(width);
    for _ in 0..width.saturating_sub(window.len()) {
        spans.push(Span::styled(" ", Style::default()));
    }
    for &v in &window {
        let t = (v as f64 / max as f64).clamp(0.0, 1.0);
        let idx = ((t * 7.0).round() as usize).min(7);
        spans.push(Span::styled(
            SPARK_CHARS[idx].to_string(),
            Style::default().fg(ramp(t)),
        ));
    }
    spans
}

fn temp_span(v: f64) -> Span<'static> {
    // scale tuned for silicon: green below ~55, amber to ~80, red past it
    Span::styled(format!("{v:.1}°C"), Style::default().fg(ramp((v - 30.0) / 60.0)))
}

fn spinner() -> char {
    let ms = SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis();
    SPINNER[(ms / 120) as usize % SPINNER.len()]
}

fn chip_lines(chip: &HwmonChip, indent: &str) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    let key = |l: &str| Span::styled(format!("{indent}{l:<16}"), Style::default().fg(ACCENT));
    for (l, v) in &chip.temps {
        lines.push(Line::from(vec![key(l), temp_span(*v)]));
    }
    for (l, v) in &chip.fans {
        let style = if *v == 0 { Style::default().fg(MUTED) } else { Style::default().fg(VALUE) };
        lines.push(Line::from(vec![key(l), Span::styled(format!("{v} RPM"), style)]));
    }
    for (l, v) in &chip.freqs {
        lines.push(Line::from(vec![key(l), Span::styled(format!("{v:.0} MHz"), Style::default().fg(VALUE))]));
    }
    for (l, v) in &chip.volts {
        lines.push(Line::from(vec![key(l), Span::styled(format!("{v:.3} V"), Style::default().fg(VALUE))]));
    }
    for (l, v) in &chip.powers {
        lines.push(Line::from(vec![key(l), Span::styled(format!("{v:.1} W"), Style::default().fg(POWER))]));
    }
    lines
}

pub fn system_tab(
    statics: &StaticInfo,
    sampler: &mut Sampler,
    updates: &Arc<Mutex<UpdatesInfo>>,
) -> Text<'static> {
    let (load, sched) = collect::loadavg();
    let mut lines = vec![
        header("Host System"),
        kv("OS", statics.os_name.clone()),
        kv("Hostname", statics.hostname.clone()),
        kv("Kernel", statics.kernel.clone()),
        kv("Architecture", statics.arch.clone()),
        kv("Uptime", collect::uptime()),
        blank(),
        header("System Load"),
        kv("Load Average", load),
        kv("Processes", format!("{}  (runnable/total: {sched})", collect::process_count())),
        blank(),
        header("CPU"),
        kv("Model", statics.cpu_model.clone()),
        kv("Frequency", collect::cpu_mhz()),
        kv("Governor", collect::cpu_governor()),
    ];

    match (sampler.cpu_power, sampler.rapl_denied) {
        (Some(w), _) => lines.push(kv_spans(
            "Package Power",
            vec![Span::styled(format!("{w:.1} W"), Style::default().fg(POWER))],
        )),
        (None, true) => lines.push(kv_spans(
            "Package Power",
            vec![Span::styled(
                "n/a - RAPL is root-only (fix: sudo setcap cap_dac_read_search=ep sysinfo)",
                Style::default().fg(MUTED),
            )],
        )),
        _ => {}
    }

    lines.push(kv_spans("Load", bar_spans(sampler.cpu_load, 30)));
    lines.push(kv_spans("History (2min)", spark_spans(&sampler.cpu_history, 48, Some(100))));

    let temps = sampler.cpu_chip().map(|c| c.temps).unwrap_or_default();
    if !temps.is_empty() {
        let mut spans = Vec::new();
        for (l, v) in &temps {
            spans.push(Span::styled(format!("{l} "), Style::default().fg(MUTED)));
            spans.push(temp_span(*v));
            spans.push(Span::raw("  "));
        }
        lines.push(kv_spans("Temps", spans));
    }

    lines.push(blank());
    let (update_lines, status) = match updates.lock() {
        Ok(lock) => {
            let status = if lock.checking {
                format!("{} checking", spinner())
            } else if let Some(at) = lock.last_checked {
                format!("checked {}s ago", Instant::now().duration_since(at).as_secs())
            } else {
                String::new()
            };
            (lock.lines.clone(), status)
        }
        Err(_) => (vec!["unavailable".to_string()], String::new()),
    };
    lines.push(header(&format!("Available Updates [r] {status}")));
    if update_lines.is_empty() {
        lines.push(dim(format!("{} gathering...", spinner())));
    }
    for l in update_lines {
        let color = if l.contains("up to date") {
            GOOD
        } else if l.contains("updates") {
            WARN
        } else {
            MUTED
        };
        lines.push(Line::from(Span::styled(l, Style::default().fg(color))));
    }

    lines.push(blank());
    lines.push(dim("Tabs: [1-6] or [h/l]  |  Quit: [q]/[Esc]"));
    Text::from(lines)
}

pub fn cores_tab(sampler: &mut Sampler) -> Text<'static> {
    let mut lines = vec![header("Per-Core CPU Utilization")];
    for (idx, &load) in sampler.core_loads.iter().enumerate() {
        let mut spans = vec![Span::styled(format!("Core {idx:2} "), Style::default().fg(ACCENT))];
        spans.extend(bar_spans(load, 30));
        lines.push(Line::from(spans));
    }

    if let Some(chip) = sampler.cpu_chip() {
        let per_core: Vec<&(String, f64)> =
            chip.temps.iter().filter(|(l, _)| l.starts_with("Core ")).collect();
        lines.push(blank());
        if !per_core.is_empty() {
            lines.push(header("Per-Core Temperatures"));
            for (l, v) in per_core {
                lines.push(Line::from(vec![
                    Span::styled(format!("{l:<8} "), Style::default().fg(ACCENT)),
                    temp_span(*v),
                ]));
            }
        } else if !chip.temps.is_empty() {
            lines.push(header(&format!("CPU Temperatures [{}]", chip.name)));
            for (l, v) in &chip.temps {
                lines.push(Line::from(vec![
                    Span::styled(format!("{l:<8} "), Style::default().fg(ACCENT)),
                    temp_span(*v),
                ]));
            }
            lines.push(blank());
            lines.push(dim(format!(
                "({} does not expose per-core sensors; these cover the package/CCDs)",
                chip.name
            )));
        }
    }
    Text::from(lines)
}

pub fn memory_tab(sampler: &mut Sampler) -> Text<'static> {
    let (total, used) = collect::ram_usage();
    let total_gb = total as f64 / 1024.0 / 1024.0 / 1024.0;
    let used_gb = used as f64 / 1024.0 / 1024.0 / 1024.0;
    let pct = if total > 0 { used as f64 / total as f64 * 100.0 } else { 0.0 };

    let mut lines = vec![
        header("Memory (RAM)"),
        kv("Usage", format!("{used_gb:.2} GB / {total_gb:.2} GB")),
        Line::from(bar_spans(pct, 40)),
        blank(),
        header("ZRAM Swap"),
    ];
    for l in collect::zram_info().lines() {
        lines.push(Line::from(Span::styled(l.to_string(), Style::default().fg(VALUE))));
    }
    lines.push(blank());
    lines.push(header("Disks & Partitions"));
    for (i, l) in sampler.disks().lines().enumerate() {
        let style = if i == 0 { Style::default().fg(ACCENT) } else { Style::default().fg(VALUE) };
        lines.push(Line::from(Span::styled(l.to_string(), style)));
    }
    Text::from(lines)
}

pub fn gpu_tab(statics: &StaticInfo, sampler: &mut Sampler) -> Text<'static> {
    if statics.gpus.is_empty() {
        return Text::from("No GPUs detected under /sys/class/drm");
    }
    let mut lines = Vec::new();
    for (idx, gpu) in statics.gpus.iter().enumerate() {
        lines.push(header(&format!("GPU {}: {} [{}]", idx + 1, gpu.name, gpu.driver)));
        if let Some((total, used)) = collect::amd_vram(&gpu.dev_path) {
            let pct = used as f64 / total as f64 * 100.0;
            let mut spans = vec![Span::styled(format!("  {:<16}", "VRAM"), Style::default().fg(ACCENT))];
            spans.extend(bar_spans(pct, 20));
            spans.push(Span::styled(
                format!("  {:.0} / {:.0} MB", used as f64 / 1048576.0, total as f64 / 1048576.0),
                Style::default().fg(VALUE),
            ));
            lines.push(Line::from(spans));
        }
        match gpu.hwmon_id.as_deref().and_then(|id| sampler.hwmon_by_id(id)) {
            Some(c) if !c.is_empty() => lines.extend(chip_lines(&c, "  ")),
            _ => lines.push(dim("  no sensor data exposed")),
        }
        lines.push(blank());
    }
    Text::from(lines)
}

pub fn net_tab(statics: &StaticInfo, sampler: &mut Sampler) -> Text<'static> {
    let mut lines = vec![header("Network Interfaces")];
    let ips = sampler.ips().clone();
    let mut ifaces: Vec<String> = sampler.net_speeds.keys().filter(|i| *i != "lo").cloned().collect();
    ifaces.sort();
    if ifaces.is_empty() {
        lines.push(dim("  none found"));
    }
    for iface in &ifaces {
        if let Some(&(rx, tx)) = sampler.net_speeds.get(iface) {
            let ip = ips.get(iface).map(String::as_str).unwrap_or("no IPv4");
            let rate = |v: f64| {
                let style = if v >= 1.0 { Style::default().fg(GOOD) } else { Style::default().fg(MUTED) };
                Span::styled(format!("{v:9.1} KB/s"), style)
            };
            lines.push(Line::from(vec![
                Span::styled(format!("  {iface:<10} "), Style::default().fg(ACCENT)),
                Span::styled("▼ ", Style::default().fg(MUTED)),
                rate(rx),
                Span::styled("  ▲ ", Style::default().fg(MUTED)),
                rate(tx),
                Span::styled(format!("   {ip}"), Style::default().fg(VALUE)),
            ]));
        }
    }

    lines.push(blank());
    lines.push(header("Throughput (2min, all interfaces)"));
    lines.push(kv_spans("Download", spark_spans(&sampler.rx_history, 48, None)));
    lines.push(kv_spans("Upload", spark_spans(&sampler.tx_history, 48, None)));

    lines.push(blank());
    lines.push(header("Wi-Fi Hardware Capabilities"));
    lines.push(kv("Max Standard", statics.wifi_standard.clone()));
    lines.push(kv("Bands", statics.wifi_bands.clone()));
    Text::from(lines)
}

pub fn sensors_tab(statics: &StaticInfo, sampler: &mut Sampler) -> Text<'static> {
    let gpu_names: std::collections::HashMap<&str, &str> = statics
        .gpus
        .iter()
        .filter_map(|g| g.hwmon_id.as_deref().map(|id| (id, g.name.as_str())))
        .collect();
    let chips: Vec<HwmonChip> = sampler.hwmon().to_vec();

    let mut lines = vec![header("All Hardware Sensors (hwmon)"), blank()];
    if chips.is_empty() {
        lines.push(dim("no hwmon chips found"));
        return Text::from(lines);
    }
    let mut fan_count = 0usize;
    for chip in &chips {
        fan_count += chip.fans.len();
        let context = if CPU_CHIPS.contains(&chip.name.as_str()) {
            " · CPU".to_string()
        } else if let Some(gpu) = gpu_names.get(chip.id.as_str()) {
            format!(" · {gpu}")
        } else {
            String::new()
        };
        lines.push(Line::from(vec![
            Span::styled(format!("[{}]", chip.name), Style::default().fg(TITLE).add_modifier(Modifier::BOLD)),
            Span::styled(context, Style::default().fg(VALUE)),
            Span::styled(format!(" ({})", chip.id), Style::default().fg(MUTED)),
        ]));
        lines.extend(chip_lines(chip, "  "));
        lines.push(blank());
    }

    if fan_count == 0 {
        for l in [
            "No fan or pump RPM sensors found.",
            "Case fans, CPU fans and pumps are usually read by the motherboard's",
            "Super I/O chip, whose kernel driver may not be loaded. Try:",
            "  sudo modprobe nct6775   (Nuvoton, most ASRock/ASUS/MSI boards)",
            "or run sensors-detect from lm_sensors to identify the right driver.",
        ] {
            lines.push(dim(l));
        }
    }
    Text::from(lines)
}
