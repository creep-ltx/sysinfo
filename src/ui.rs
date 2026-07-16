//! Tab content rendering: pure string builders over collected data.

use std::sync::{Arc, Mutex};
use std::time::Instant;

use crate::collect::{self, HwmonChip, Sampler, StaticInfo, UpdatesInfo, CPU_CHIPS};

pub const TABS: &[&str] = &[
    "[1] System & Load",
    "[2] CPU Cores",
    "[3] Memory & Disk",
    "[4] GPUs",
    "[5] Net & Sensors",
];

fn bar(percentage: f64, width: usize) -> String {
    let filled = ((percentage / 100.0) * width as f64).round().clamp(0.0, width as f64) as usize;
    format!("[{}{}] {:.1}%", "█".repeat(filled), "░".repeat(width - filled), percentage)
}

fn chip_lines(chip: &HwmonChip, indent: &str) -> String {
    let mut out = String::new();
    for (label, v) in &chip.temps {
        out.push_str(&format!("{indent}{label:<14} {v:.1}°C\n"));
    }
    for (label, v) in &chip.fans {
        out.push_str(&format!("{indent}{label:<14} {v} RPM\n"));
    }
    for (label, v) in &chip.freqs {
        out.push_str(&format!("{indent}{label:<14} {v:.0} MHz\n"));
    }
    for (label, v) in &chip.volts {
        out.push_str(&format!("{indent}{label:<14} {v:.3} V\n"));
    }
    for (label, v) in &chip.powers {
        out.push_str(&format!("{indent}{label:<14} {v:.1} W\n"));
    }
    out
}

pub fn system_tab(statics: &StaticInfo, sampler: &mut Sampler, updates: &Arc<Mutex<UpdatesInfo>>) -> String {
    let (load, sched) = collect::loadavg();
    let cpu_temps = sampler
        .cpu_chip()
        .map(|c| {
            c.temps
                .iter()
                .map(|(l, v)| format!("{l}: {v:.1}°C"))
                .collect::<Vec<_>>()
                .join("  ")
        })
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "N/A".to_string());

    let updates_str = match updates.lock() {
        Ok(lock) => {
            if lock.lines.is_empty() {
                "Checking...".to_string()
            } else {
                let status = match (lock.checking, lock.last_checked) {
                    (true, _) => " (refreshing...)".to_string(),
                    (false, Some(at)) => format!(" (checked {}s ago)", Instant::now().duration_since(at).as_secs()),
                    (false, None) => String::new(),
                };
                format!("{}{}", lock.lines.join("\n "), status)
            }
        }
        Err(_) => "unavailable".to_string(),
    };

    format!(
        "--- Host System ---\n\
         OS:               {}\n\
         Hostname:         {}\n\
         Kernel:           {}\n\
         Architecture:     {}\n\
         Uptime:           {}\n\n\
         --- System Load ---\n\
         Load Average:     {}\n\
         Processes:        {}  (runnable/total: {})\n\n\
         --- CPU ---\n\
         Model:            {}\n\
         Frequency:        {}\n\
         Governor:         {}\n\
         Load:             {}\n\
         Temps:            {}\n\n\
         --- Available Updates [r to refresh] ---\n \
         {}\n\n\
         Tabs: [1-5] or [h/l]  |  Quit: [q]/[Esc]",
        statics.os_name,
        statics.hostname,
        statics.kernel,
        statics.arch,
        collect::uptime(),
        load,
        collect::process_count(),
        sched,
        statics.cpu_model,
        collect::cpu_mhz(),
        collect::cpu_governor(),
        bar(sampler.cpu_load, 20),
        cpu_temps,
        updates_str,
    )
}

pub fn cores_tab(sampler: &Sampler) -> String {
    let mut out = String::from("--- Per-Core CPU Utilization ---\n");
    for (idx, &load) in sampler.core_loads.iter().enumerate() {
        out.push_str(&format!("Core {idx:2}: {}\n", bar(load, 30)));
    }
    out
}

pub fn memory_tab(sampler: &mut Sampler) -> String {
    let (total, used) = collect::ram_usage();
    let total_gb = total as f64 / 1024.0 / 1024.0 / 1024.0;
    let used_gb = used as f64 / 1024.0 / 1024.0 / 1024.0;
    let pct = if total > 0 { used as f64 / total as f64 * 100.0 } else { 0.0 };

    format!(
        "--- Memory (RAM) ---\n\
         Usage: {used_gb:.2} GB / {total_gb:.2} GB\n\
         {}\n\n\
         --- ZRAM Swap ---\n\
         {}\n\n\
         --- Disks & Partitions ---\n\
         {}",
        bar(pct, 30),
        collect::zram_info(),
        sampler.disks(),
    )
}

pub fn gpu_tab(statics: &StaticInfo, sampler: &mut Sampler) -> String {
    if statics.gpus.is_empty() {
        return "No GPUs detected under /sys/class/drm".to_string();
    }
    let mut out = String::new();
    for (idx, gpu) in statics.gpus.iter().enumerate() {
        out.push_str(&format!("--- GPU {}: {} [{}] ---\n", idx + 1, gpu.name, gpu.driver));
        if let Some((total, used)) = collect::amd_vram(&gpu.dev_path) {
            let pct = used as f64 / total as f64 * 100.0;
            out.push_str(&format!(
                "  {:<14} {:.0} MB / {:.0} MB ({:.1}%)\n",
                "VRAM",
                used as f64 / 1024.0 / 1024.0,
                total as f64 / 1024.0 / 1024.0,
                pct
            ));
        }
        let chip = gpu.hwmon_id.as_deref().and_then(|id| sampler.hwmon_by_id(id));
        match chip {
            Some(c) if !c.is_empty() => out.push_str(&chip_lines(&c, "  ")),
            _ => out.push_str("  no sensor data exposed\n"),
        }
        out.push('\n');
    }
    out
}

pub fn net_tab(statics: &StaticInfo, sampler: &mut Sampler) -> String {
    let mut out = String::from("--- Network Interfaces ---\n");
    let ips = sampler.ips().clone();
    let mut ifaces: Vec<&String> = sampler.net_speeds.keys().filter(|i| *i != "lo").collect();
    ifaces.sort();
    if ifaces.is_empty() {
        out.push_str("  none found\n");
    }
    for iface in ifaces {
        if let Some(&(rx, tx)) = sampler.net_speeds.get(iface) {
            let ip = ips.get(iface).map(String::as_str).unwrap_or("no IPv4");
            out.push_str(&format!(
                "  {iface:<10} down {rx:9.1} KB/s | up {tx:9.1} KB/s | {ip}\n"
            ));
        }
    }

    out.push_str(&format!(
        "\n--- Wi-Fi Hardware Capabilities ---\n\
         Max Standard:    {}\n\
         Bands:           {}\n",
        statics.wifi_standard, statics.wifi_bands
    ));

    // every hwmon chip not already shown on the CPU or GPU tabs
    let gpu_ids: Vec<String> = statics.gpus.iter().filter_map(|g| g.hwmon_id.clone()).collect();
    let others: Vec<HwmonChip> = sampler
        .hwmon()
        .iter()
        .filter(|c| !CPU_CHIPS.contains(&c.name.as_str()) && !gpu_ids.contains(&c.id))
        .cloned()
        .collect();
    out.push_str("\n--- Other Sensors ---\n");
    if others.is_empty() {
        out.push_str("  none found\n");
    }
    for chip in &others {
        out.push_str(&format!("  [{}]\n", chip.name));
        out.push_str(&chip_lines(chip, "    "));
    }
    out
}
