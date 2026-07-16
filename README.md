# SysInfo 📊

A real-time hardware telemetry and package update status TUI dashboard in Rust.
Everything is auto-detected at runtime — no hardware assumptions baked in.

## Tech Stack
- **Language:** Rust (stdlib + `ratatui` + `crossterm` only)
- **Data sources:** `/proc` and `/sys` (hwmon, DRM, block) read directly;
  `lspci`, `iw`, `ip`, and `lsblk` where sysfs has no equivalent.

## Tabs
1. **System & Load** — host info, load average, CPU model/frequency/governor/temps,
   pending package updates (pacman, AUR, flatpak, rustup, cargo, pipx, pip, npm, gem, go, composer, deno — all auto-detected, read-only; updating is sysup's job).
2. **CPU Cores** — per-core utilization bars, plus per-core temperatures where the CPU exposes them (Intel coretemp); AMD k10temp shows package/CCD temps.
3. **Memory & Disk** — RAM usage, every ZRAM device, block device tree.
4. **GPUs** — every card under `/sys/class/drm`, found by driver symlink
   (amdgpu, xe, i915, nouveau, ...), named via `lspci`, with its own hwmon
   sensors (temps, fans, clocks, voltages, power) and VRAM usage where exposed.
5. **Network** — per-interface bandwidth, IPv4, Wi-Fi hardware capabilities.
6. **All Sensors** — every hwmon chip in the system (CPU, GPUs, NVMe, RAM SPD,
   NICs, Super I/O, ...) with temps, fan/pump RPMs, clocks, voltages and power;
   hints at the missing Super I/O driver when no fan sensors are exposed.

## Design
- **No hardcoding:** sensors come from hwmon sysfs enumeration (not parsed
  `sensors` output), GPUs from DRM driver symlinks (not fixed card numbers),
  the AUR helper is detected (paru/yay/pikaur).
- **Lightweight:** static facts (hostname, kernel, CPU model, GPU names, Wi-Fi
  caps) are collected once at startup. The render loop only re-reads cheap
  /proc//sys files; hwmon is cached for 2s, `lsblk` for 5s, IPs for 10s.
  Steady state spawns roughly one process every few seconds, not several per frame.
- **Safe update checks:** read-only commands in a background thread, never
  elevated; npm is only counted when its global prefix is user-owned,
  otherwise it is reported as pacman-managed.
- **Robust terminal handling:** raw mode and the alternate screen are restored
  via an RAII guard and a panic hook, so a crash can't wedge the shell.

## Keys
`1-6` / `h` `l` — switch tabs · `r` — refresh update counts · `q` / `Esc` / `Ctrl-C` — quit
