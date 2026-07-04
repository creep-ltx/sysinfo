use std::collections::HashMap;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout},
    style::{Color, Modifier, Style},
    widgets::{Block, Borders, Paragraph, Tabs},
    Terminal,
};
use crossterm::{
    event::{self, Event, KeyCode},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};

#[derive(Default)]
struct SensorsInfo {
    cpu_tctl: String,
    cpu_tccd1: String,
    gpu_intel_fan1: String,
    gpu_intel_fan2: String,
    gpu_intel_pkg: String,
    gpu_intel_vram: String,
    gpu_amd_edge: String,
    gpu_amd_sclk: String,
    gpu_amd_vddgfx: String,
    nvme_temp: String,
    ram_temps: Vec<String>,
    eth_temp: String,
    wifi_temp: String,
}

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

#[derive(Clone, Default)]
struct UpdatesInfo {
    pacman: String,
    aur: String,
    npm: String,
    checking: bool,
    last_checked: Option<Instant>,
}

struct SystemState {
    prev_cpu: Vec<CpuTimes>,
    prev_global_cpu: CpuTimes,
    prev_net: HashMap<String, NetBytes>,
    last_update: Instant,
    global_cpu_load: f64,
    core_cpu_loads: Vec<f64>,
    net_speeds: HashMap<String, (f64, f64)>, // speed in KB/s: (download, upload)
    wifi_standard: String,
    wifi_bands: String,
    updates: Arc<Mutex<UpdatesInfo>>,
}

impl Default for SystemState {
    fn default() -> Self {
        Self {
            prev_cpu: Vec::new(),
            prev_global_cpu: CpuTimes::default(),
            prev_net: HashMap::new(),
            last_update: Instant::now(),
            global_cpu_load: 0.0,
            core_cpu_loads: Vec::new(),
            net_speeds: HashMap::new(),
            wifi_standard: "Unknown".to_string(),
            wifi_bands: "Unknown".to_string(),
            updates: Arc::new(Mutex::new(UpdatesInfo::default())),
        }
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = std::io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let tabs_list = vec![
        "[1] System & Load",
        "[2] CPU Core Loads",
        "[3] Memory, ZRAM & Disk",
        "[4] GPUs (Arc / AMD)",
        "[5] Net & Other Sensors",
    ];
    let mut active_tab = 0;
    let (wifi_std, wifi_bnd) = get_wifi_capabilities();
    let updates_info = Arc::new(Mutex::new(UpdatesInfo::default()));
    let mut state = SystemState {
        wifi_standard: wifi_std,
        wifi_bands: wifi_bnd,
        updates: updates_info.clone(),
        ..SystemState::default()
    };

    // Spawn background updates checker thread
    let state_updates = updates_info.clone();
    std::thread::spawn(move || {
        loop {
            trigger_updates_check(state_updates.clone());
            std::thread::sleep(Duration::from_secs(600));
        }
    });

    // Do an initial sample to fill state history
    update_system_metrics(&mut state);

    loop {
        // Collect latest metrics
        update_system_metrics(&mut state);
        let sensors = parse_sensors();

        // Create layout
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .margin(1)
            .constraints([Constraint::Length(3), Constraint::Min(0)])
            .split(terminal.size()?);

        // Fetch display text
        let content_str = match active_tab {
            0 => get_system_tab(&sensors, &state),
            1 => get_cpu_cores_tab(&state),
            2 => get_memory_disk_tab(&sensors),
            3 => get_gpu_tab(&sensors),
            4 => get_net_sensors_tab(&sensors, &state),
            _ => "Unknown Tab".to_string(),
        };

        // Render widgets
        terminal.draw(|f| {
            let tabs_widget = Tabs::new(tabs_list.clone())
                .block(Block::default().borders(Borders::ALL).title("Hardware Dashboard"))
                .select(active_tab)
                .style(Style::default().fg(Color::Cyan))
                .highlight_style(Style::default().fg(Color::Yellow).add_modifier(Modifier::BOLD));

            let content_widget = Paragraph::new(content_str)
                .block(Block::default().borders(Borders::ALL).title("Details"))
                .style(Style::default().fg(Color::White));

            f.render_widget(tabs_widget, chunks[0]);
            f.render_widget(content_widget, chunks[1]);
        })?;

        // Handle input
        if event::poll(Duration::from_millis(500))? {
            if let Event::Key(key) = event::read()? {
                if key.kind == event::KeyEventKind::Press {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => break,
                        KeyCode::Char('r') => {
                            let state_updates = state.updates.clone();
                            std::thread::spawn(move || {
                                trigger_updates_check(state_updates);
                            });
                        }
                        KeyCode::Left | KeyCode::Char('h') => {
                            if active_tab > 0 {
                                active_tab -= 1;
                            }
                        }
                        KeyCode::Right | KeyCode::Char('l') => {
                            if active_tab < tabs_list.len() - 1 {
                                active_tab += 1;
                            }
                        }
                        KeyCode::Char('1') => active_tab = 0,
                        KeyCode::Char('2') => active_tab = 1,
                        KeyCode::Char('3') => active_tab = 2,
                        KeyCode::Char('4') => active_tab = 3,
                        KeyCode::Char('5') => active_tab = 4,
                        _ => {}
                    }
                }
            }
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(std::io::stdout(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;

    Ok(())
}

fn run_cmd(cmd: &str) -> String {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        return String::new();
    }
    let mut command = Command::new(parts[0]);
    if parts.len() > 1 {
        command.args(&parts[1..]);
    }
    match command.output() {
        Ok(output) => String::from_utf8_lossy(&output.stdout).to_string(),
        Err(e) => format!("Failed to execute command: {}", e),
    }
}

fn parse_sensors() -> SensorsInfo {
    let output = run_cmd("sensors");
    let mut info = SensorsInfo::default();
    let mut current_section = String::new();
    let mut prev_line = String::new();
    
    for line in output.lines() {
        let line_trimmed = line.trim();
        if line_trimmed.is_empty() {
            continue;
        }
        
        if line.contains("Adapter:") {
            current_section = prev_line.split_whitespace().next().unwrap_or("").to_string();
            continue;
        }
        
        prev_line = line.to_string();
        
        let parts: Vec<&str> = line_trimmed.split(':').collect();
        if parts.len() < 2 {
            continue;
        }
        let label = parts[0].trim();
        let val_str = parts[1].trim();
        
        if current_section.starts_with("k10temp") {
            if label == "Tctl" {
                info.cpu_tctl = val_str.to_string();
            } else if label == "Tccd1" {
                info.cpu_tccd1 = val_str.to_string();
            }
        } else if current_section.starts_with("xe") {
            if label == "fan1" {
                info.gpu_intel_fan1 = val_str.to_string();
            } else if label == "fan2" {
                info.gpu_intel_fan2 = val_str.to_string();
            } else if label == "pkg" {
                info.gpu_intel_pkg = val_str.to_string();
            } else if label == "vram" {
                info.gpu_intel_vram = val_str.to_string();
            }
        } else if current_section.starts_with("amdgpu") {
            if label == "edge" {
                info.gpu_amd_edge = val_str.to_string();
            } else if label == "sclk" {
                info.gpu_amd_sclk = val_str.to_string();
            } else if label == "vddgfx" {
                info.gpu_amd_vddgfx = val_str.to_string();
            }
        } else if current_section.starts_with("nvme") {
            if label == "Composite" {
                info.nvme_temp = val_str.to_string();
            }
        } else if current_section.starts_with("spd5118") {
            if label == "temp1" {
                info.ram_temps.push(val_str.to_string());
            }
        } else if current_section.starts_with("r8169") {
            if label == "temp1" {
                info.eth_temp = val_str.to_string();
            }
        } else if current_section.starts_with("mt7921") {
            if label == "temp1" {
                info.wifi_temp = val_str.to_string();
            }
        }
    }
    info
}

fn get_cpu_times() -> (CpuTimes, Vec<CpuTimes>) {
    let mut global = CpuTimes::default();
    let mut cores = Vec::new();
    if let Ok(content) = std::fs::read_to_string("/proc/stat") {
        for line in content.lines() {
            if line.starts_with("cpu") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 5 {
                    let user = parts[1].parse::<u64>().unwrap_or(0);
                    let nice = parts[2].parse::<u64>().unwrap_or(0);
                    let system = parts[3].parse::<u64>().unwrap_or(0);
                    let idle = parts[4].parse::<u64>().unwrap_or(0);
                    let iowait = parts.get(5).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                    let irq = parts.get(6).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                    let softirq = parts.get(7).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                    let steal = parts.get(8).and_then(|s| s.parse::<u64>().ok()).unwrap_or(0);
                    
                    let idle_time = idle + iowait;
                    let total_time = user + nice + system + idle_time + irq + softirq + steal;
                    
                    let times = CpuTimes { idle: idle_time, total: total_time };
                    if parts[0] == "cpu" {
                        global = times;
                    } else {
                        cores.push(times);
                    }
                }
            }
        }
    }
    (global, cores)
}

fn calculate_load(prev: CpuTimes, curr: CpuTimes) -> f64 {
    let total_diff = curr.total.saturating_sub(prev.total);
    let idle_diff = curr.idle.saturating_sub(prev.idle);
    if total_diff == 0 {
        return 0.0;
    }
    let active_diff = total_diff.saturating_sub(idle_diff);
    (active_diff as f64 / total_diff as f64) * 100.0
}

fn get_net_bytes() -> HashMap<String, NetBytes> {
    let mut map = HashMap::new();
    if let Ok(content) = std::fs::read_to_string("/proc/net/dev") {
        for line in content.lines() {
            let line_trimmed = line.trim();
            if line_trimmed.contains(':') {
                let parts: Vec<&str> = line_trimmed.split(':').collect();
                if parts.len() >= 2 {
                    let interface = parts[0].trim().to_string();
                    let stats_parts: Vec<&str> = parts[1].split_whitespace().collect();
                    if stats_parts.len() >= 9 {
                        let rx = stats_parts[0].parse::<u64>().unwrap_or(0);
                        let tx = stats_parts[8].parse::<u64>().unwrap_or(0);
                        map.insert(interface, NetBytes { rx, tx });
                    }
                }
            }
        }
    }
    map
}

fn get_process_count() -> usize {
    let mut count = 0;
    if let Ok(entries) = std::fs::read_dir("/proc") {
        for entry in entries {
            if let Ok(entry) = entry {
                if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                    if entry.file_name().to_string_lossy().chars().all(|c| c.is_ascii_digit()) {
                        count += 1;
                    }
                }
            }
        }
    }
    count
}

fn get_wifi_capabilities() -> (String, String) {
    let output = run_cmd("iw phy");
    if output.is_empty() {
        return ("N/A".to_string(), "N/A".to_string());
    }
    
    let mut has_ht = false;
    let mut has_vht = false;
    let mut has_he = false;
    let mut has_eht = false;
    
    let mut has_2ghz = false;
    let mut has_5ghz = false;
    let mut has_6ghz = false;
    
    for line in output.lines() {
        if line.contains("HT Capabilities") {
            has_ht = true;
        } else if line.contains("VHT Capabilities") {
            has_vht = true;
        } else if line.contains("HE Capabilities") || line.contains("HE MAC Capabilities") {
            has_he = true;
        } else if line.contains("EHT Capabilities") {
            has_eht = true;
        }
        
        if line.contains("MHz") {
            if let Some(pos) = line.find("MHz") {
                let prefix = &line[..pos];
                if let Some(last_word) = prefix.split_whitespace().last() {
                    if let Ok(freq) = last_word.parse::<f64>() {
                        if freq >= 2400.0 && freq < 2500.0 {
                            has_2ghz = true;
                        } else if freq >= 5000.0 && freq < 5900.0 {
                            has_5ghz = true;
                        } else if freq >= 5900.0 && freq < 7200.0 {
                            has_6ghz = true;
                        }
                    }
                }
            }
        }
    }
    
    let standard = if has_eht {
        "Wi-Fi 7 (802.11be)"
    } else if has_he {
        if has_6ghz {
            "Wi-Fi 6E (802.11ax)"
        } else {
            "Wi-Fi 6 (802.11ax)"
        }
    } else if has_vht {
        "Wi-Fi 5 (802.11ac)"
    } else if has_ht {
        "Wi-Fi 4 (802.11n)"
    } else {
        "Legacy (802.11a/b/g)"
    };
    
    let mut bands = Vec::new();
    if has_2ghz { bands.push("2.4 GHz"); }
    if has_5ghz { bands.push("5 GHz"); }
    if has_6ghz { bands.push("6 GHz"); }
    
    let bands_str = if bands.is_empty() {
        "None".to_string()
    } else {
        bands.join(", ")
    };
    
    (standard.to_string(), bands_str)
}

fn get_cpu_governor() -> String {
    std::fs::read_to_string("/sys/devices/system/cpu/cpu0/cpufreq/scaling_governor")
        .unwrap_or_else(|_| "Unknown".to_string())
        .trim()
        .to_string()
}

fn get_amd_vram_usage() -> (u64, u64) {
    let read_val = |file| {
        std::fs::read_to_string(format!("/sys/class/drm/card1/device/{}", file))
            .unwrap_or_default()
            .trim()
            .parse::<u64>()
            .unwrap_or(0)
    };
    (read_val("mem_info_vram_total"), read_val("mem_info_vram_used"))
}

fn get_local_ip(interface: &str) -> String {
    let output = run_cmd(&format!("ip -4 addr show dev {}", interface));
    for line in output.lines() {
        if line.trim().starts_with("inet ") {
            let parts: Vec<&str> = line.trim().split_whitespace().collect();
            if parts.len() >= 2 {
                return parts[1].to_string();
            }
        }
    }
    "Not Assigned".to_string()
}

fn get_os_pretty_name() -> String {
    if let Ok(content) = std::fs::read_to_string("/etc/os-release") {
        for line in content.lines() {
            if line.starts_with("PRETTY_NAME=") {
                let val = line.split('=').nth(1).unwrap_or("").trim_matches('"');
                return val.to_string();
            }
        }
    }
    "Linux".to_string()
}

fn trigger_updates_check(state_updates: Arc<Mutex<UpdatesInfo>>) {
    {
        if let Ok(mut lock) = state_updates.lock() {
            if lock.checking {
                return; // Already checking
            }
            lock.checking = true;
        }
    }
    
    let pacman_count = run_cmd("checkupdates")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
        
    let aur_count = run_cmd("paru -Qum")
        .lines()
        .filter(|l| !l.trim().is_empty())
        .count();
        
    let npm_outdated = run_cmd("npm outdated -g");
    let npm_count = npm_outdated
        .lines()
        .skip(1)
        .filter(|l| !l.trim().is_empty())
        .count();
        
    if let Ok(mut lock) = state_updates.lock() {
        lock.pacman = pacman_count.to_string();
        lock.aur = aur_count.to_string();
        lock.npm = npm_count.to_string();
        lock.checking = false;
        lock.last_checked = Some(Instant::now());
    }
}

fn get_zram_info() -> String {
    let dev = "zram0";
    let path_prefix = format!("/sys/block/{}/", dev);
    if !std::path::Path::new(&path_prefix).exists() {
        return "ZRAM: Device zram0 not found".to_string();
    }
    
    let read_sys_u64 = |file_name: &str| -> u64 {
        std::fs::read_to_string(format!("{}{}", path_prefix, file_name))
            .unwrap_or_default()
            .trim()
            .parse::<u64>()
            .unwrap_or(0)
    };
    
    let disksize = read_sys_u64("disksize");
    let orig_size = read_sys_u64("orig_data_size");
    let compr_size = read_sys_u64("compr_data_size");
    
    let algo_data = std::fs::read_to_string(format!("{}{}", path_prefix, "comp_algorithm"))
        .unwrap_or_default()
        .trim()
        .to_string();
        
    let active_algo = algo_data.split_whitespace()
        .find(|s| s.starts_with('[') && s.ends_with(']'))
        .map(|s| s.trim_matches(|c| c == '[' || c == ']'))
        .unwrap_or("unknown");
        
    let disk_gb = disksize as f64 / 1024.0 / 1024.0 / 1024.0;
    let orig_mb = orig_size as f64 / 1024.0 / 1024.0;
    let compr_mb = compr_size as f64 / 1024.0 / 1024.0;
    let saved_mb = orig_mb - compr_mb;
    let ratio = if compr_size > 0 { orig_size as f64 / compr_size as f64 } else { 1.0 };
    
    format!(
        "Device Node:           {}\n\
         Virtual Swap Size:     {:.2} GB\n\
         Compression Algorithm: {}\n\
         Original Size:         {:.2} MB\n\
         Compressed RAM Size:   {:.2} MB\n\
         RAM Space Saved:       {:.2} MB\n\
         Compression Ratio:     {:.2}:1",
        dev, disk_gb, active_algo, orig_mb, compr_mb, saved_mb, ratio
    )
}

fn update_system_metrics(state: &mut SystemState) {
    let now = Instant::now();
    let elapsed = now.duration_since(state.last_update).as_secs_f64();
    if elapsed < 0.1 {
        return; // debounce
    }
    
    // CPU Load
    let (curr_global, curr_cores) = get_cpu_times();
    if state.prev_global_cpu.total > 0 {
        state.global_cpu_load = calculate_load(state.prev_global_cpu, curr_global);
    }
    state.prev_global_cpu = curr_global;
    
    state.core_cpu_loads.clear();
    for (idx, &curr_core) in curr_cores.iter().enumerate() {
        if idx < state.prev_cpu.len() {
            let load = calculate_load(state.prev_cpu[idx], curr_core);
            state.core_cpu_loads.push(load);
        } else {
            state.core_cpu_loads.push(0.0);
        }
    }
    state.prev_cpu = curr_cores;

    // Network Speeds
    let curr_net = get_net_bytes();
    for (interface, curr_bytes) in &curr_net {
        if let Some(prev_bytes) = state.prev_net.get(interface) {
            let rx_diff = curr_bytes.rx.saturating_sub(prev_bytes.rx);
            let tx_diff = curr_bytes.tx.saturating_sub(prev_bytes.tx);
            let rx_speed_kb = (rx_diff as f64 / 1024.0) / elapsed;
            let tx_speed_kb = (tx_diff as f64 / 1024.0) / elapsed;
            state.net_speeds.insert(interface.clone(), (rx_speed_kb, tx_speed_kb));
        } else {
            state.net_speeds.insert(interface.clone(), (0.0, 0.0));
        }
    }
    state.prev_net = curr_net;
    state.last_update = now;
}

fn get_cpu_speed() -> String {
    if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
        for line in content.lines() {
            if line.starts_with("cpu MHz") {
                if let Some(pos) = line.find(':') {
                    return format!("{} MHz", line[pos+1..].trim());
                }
            }
        }
    }
    "Unknown".to_string()
}

fn get_cpu_model() -> String {
    if let Ok(content) = std::fs::read_to_string("/proc/cpuinfo") {
        for line in content.lines() {
            if line.starts_with("model name") {
                if let Some(pos) = line.find(':') {
                    return line[pos+1..].trim().to_string();
                }
            }
        }
    }
    "Unknown".to_string()
}

fn get_ram_usage() -> (u64, u64) {
    let mut total = 0;
    let mut available = 0;
    if let Ok(content) = std::fs::read_to_string("/proc/meminfo") {
        for line in content.lines() {
            if line.starts_with("MemTotal:") {
                total = parse_mem_bytes(line);
            } else if line.starts_with("MemAvailable:") {
                available = parse_mem_bytes(line);
            }
        }
    }
    let used = if total >= available { total - available } else { 0 };
    (total, used)
}

fn parse_mem_bytes(line: &str) -> u64 {
    let parts: Vec<&str> = line.split_whitespace().collect();
    if parts.len() >= 2 {
        if let Ok(val) = parts[1].parse::<u64>() {
            return val * 1024; // KB to Bytes
        }
    }
    0
}

fn make_bar_graph(percentage: f64, width: usize) -> String {
    let filled_width = ((percentage / 100.0) * width as f64).round() as usize;
    let filled = "█".repeat(filled_width);
    let empty = "░".repeat(width.saturating_sub(filled_width));
    format!("[{}{}] {:.1}%", filled, empty, percentage)
}

fn get_system_tab(sensors: &SensorsInfo, state: &SystemState) -> String {
    let hostname = run_cmd("hostname");
    let kernel = run_cmd("uname -r");
    let arch = run_cmd("uname -m");
    let uptime = run_cmd("uptime -p");
    let loadavg = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
    let load_parts: Vec<&str> = loadavg.split_whitespace().collect();
    let load_str = if load_parts.len() >= 3 {
        format!("{}  {}  {}", load_parts[0], load_parts[1], load_parts[2])
    } else {
        "Unknown".to_string()
    };
    let threads_str = if load_parts.len() >= 4 {
        load_parts[3].to_string()
    } else {
        "N/A".to_string()
    };
    
    let cpu_model = get_cpu_model();
    let cpu_speed = get_cpu_speed();
    let cpu_governor = get_cpu_governor();
    let os_name = get_os_pretty_name();
    let proc_count = get_process_count();

    let updates_str = if let Ok(lock) = state.updates.lock() {
        if lock.checking && lock.last_checked.is_none() {
            "Pacman (Official): Checking...\n\
             Paru (AUR):        Checking...\n\
             NPM (Global):      Checking...".to_string()
        } else {
            let status = if lock.checking { " (Checking...)" } else { "" };
            let ago = if let Some(last) = lock.last_checked {
                let dur = Instant::now().duration_since(last).as_secs();
                format!("checked {}s ago{}", dur, status)
            } else {
                "never checked".to_string()
            };
            format!(
                "Pacman (Official): {} updates\n\
                 Paru (AUR):        {} updates\n\
                 NPM (Global):      {} updates\n\
                 (Status: {})",
                lock.pacman, lock.aur, lock.npm, ago
            )
        }
    } else {
        "Updates telemetry lock error".to_string()
    };
    
    format!(
        "--- Host System ---\n\
         OS:              {}\n\
         Hostname:        {}\n\
         Kernel:          {}\n\
         Architecture:    {}\n\
         Uptime:          {}\n\n\
         --- System Load ---\n\
         Load Average:    {}\n\
         Active Processes:{}\n\
         Active Threads:  {}\n\n\
         --- CPU Overview ---\n\
         CPU Model:       {}\n\
         Current Speed:   {}\n\
         Scaling Governor:{}\n\
         Global CPU Load: {}\n\
         CPU Temp Tctl:   {}\n\
         CPU Temp CCD1:   {}\n\n\
         --- Available Updates [r to refresh] ---\n\
         {}\n\n\
         Use [1-5] or [h/l] to navigate tabs. Press [q] or [Esc] to exit.",
        os_name,
        hostname.trim(),
        kernel.trim(),
        arch.trim(),
        uptime.trim(),
        load_str,
        proc_count,
        threads_str,
        cpu_model,
        cpu_speed,
        cpu_governor,
        make_bar_graph(state.global_cpu_load, 20),
        if sensors.cpu_tctl.is_empty() { "N/A" } else { &sensors.cpu_tctl },
        if sensors.cpu_tccd1.is_empty() { "N/A" } else { &sensors.cpu_tccd1 },
        updates_str
    )
}

fn get_cpu_cores_tab(state: &SystemState) -> String {
    let mut out = String::new();
    out.push_str("--- Per-Core CPU Utilization ---\n");
    for (idx, &load) in state.core_cpu_loads.iter().enumerate() {
        out.push_str(&format!("Core {:2}: {}\n", idx, make_bar_graph(load, 30)));
    }
    out
}

fn get_memory_disk_tab(sensors: &SensorsInfo) -> String {
    let (total_ram, used_ram) = get_ram_usage();
    let total_gb = total_ram as f64 / 1024.0 / 1024.0 / 1024.0;
    let used_gb = used_ram as f64 / 1024.0 / 1024.0 / 1024.0;
    let disk_info = run_cmd("lsblk -o NAME,SIZE,TYPE,MOUNTPOINTS,FSUSED,FSUSE%");
    let zram_info = get_zram_info();
    
    let mut ram_temps_str = String::new();
    if sensors.ram_temps.is_empty() {
        ram_temps_str.push_str("N/A");
    } else {
        for (idx, temp) in sensors.ram_temps.iter().enumerate() {
            ram_temps_str.push_str(&format!("Module {}: {} | ", idx + 1, temp));
        }
    }

    format!(
        "--- Memory (RAM) ---\n\
         RAM Usage:     {:.2} GB / {:.2} GB ({:.1}%)\n\
         RAM SPD Temps: {}\n\n\
         --- ZRAM SWAP Status ---\n\
         {}\n\n\
         --- Disks & Partitions ---\n\
         {}",
        used_gb,
        total_gb,
        (used_gb / total_gb.max(1.0)) * 100.0,
        ram_temps_str.trim_end_matches(" | "),
        zram_info,
        disk_info
    )
}

fn get_gpu_tab(sensors: &SensorsInfo) -> String {
    let (amd_total, amd_used) = get_amd_vram_usage();
    let amd_total_mb = amd_total as f64 / 1024.0 / 1024.0;
    let amd_used_mb = amd_used as f64 / 1024.0 / 1024.0;
    let amd_pct = if amd_total > 0 { (amd_used as f64 / amd_total as f64) * 100.0 } else { 0.0 };

    format!(
        "--- GPU 1: Intel Arc B580 Graphics ---\n\
         VRAM Temp:      {}\n\
         Package Temp:   {}\n\
         Fan 1 Speed:    {}\n\
         Fan 2 Speed:    {}\n\n\
         --- GPU 2: AMD Radeon Integrated Graphics ---\n\
         VRAM Usage:     {:.2} MB / {:.2} MB ({:.1}%)\n\
         Core Clock:     {}\n\
         Edge Temp:      {}\n\
         Core Voltage:   {}",
        if sensors.gpu_intel_vram.is_empty() { "N/A" } else { &sensors.gpu_intel_vram },
        if sensors.gpu_intel_pkg.is_empty() { "N/A" } else { &sensors.gpu_intel_pkg },
        if sensors.gpu_intel_fan1.is_empty() { "N/A" } else { &sensors.gpu_intel_fan1 },
        if sensors.gpu_intel_fan2.is_empty() { "N/A" } else { &sensors.gpu_intel_fan2 },
        amd_used_mb,
        amd_total_mb,
        amd_pct,
        if sensors.gpu_amd_sclk.is_empty() { "N/A" } else { &sensors.gpu_amd_sclk },
        if sensors.gpu_amd_edge.is_empty() { "N/A" } else { &sensors.gpu_amd_edge },
        if sensors.gpu_amd_vddgfx.is_empty() { "N/A" } else { &sensors.gpu_amd_vddgfx }
    )
}

fn get_net_sensors_tab(sensors: &SensorsInfo, state: &SystemState) -> String {
    let mut net_info = String::new();
    net_info.push_str("--- Active Network Interfaces (Bandwidth) ---\n");
    let mut interfaces: Vec<&String> = state.net_speeds.keys().collect();
    interfaces.sort();
    
    let mut has_net = false;
    for &iface in &interfaces {
        if iface == "lo" {
            continue;
        }
        if let Some(&(rx, tx)) = state.net_speeds.get(iface) {
            let ip_addr = get_local_ip(iface);
            // Only show interfaces with stats to keep TUI clean
            net_info.push_str(&format!(
                "  {:7}:  Download: {:8.2} KB/s  |  Upload: {:8.2} KB/s  [IP: {}]\n",
                iface, rx, tx, ip_addr
            ));
            has_net = true;
        }
    }
    if !has_net {
        net_info.push_str("  No active interfaces found.\n");
    }

    format!(
        "{}\n\
         --- Wi-Fi Hardware Capabilities ---\n\
         Max Wi-Fi Standard:  {}\n\
         Supported Bands:     {}\n\n\
         --- Storage Sensors ---\n\
         NVMe Storage Controller Temp:       {}\n\n\
         --- Motherboard Interface Sensors ---\n\
         Realtek Ethernet Controller Temp:  {}\n\
         MediaTek Wi-Fi Controller Temp:     {}",
        net_info,
        state.wifi_standard,
        state.wifi_bands,
        if sensors.nvme_temp.is_empty() { "N/A" } else { &sensors.nvme_temp },
        if sensors.eth_temp.is_empty() { "N/A" } else { &sensors.eth_temp },
        if sensors.wifi_temp.is_empty() { "N/A" } else { &sensors.wifi_temp }
    )
}