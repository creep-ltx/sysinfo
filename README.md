# SysInfo 📊

A real-time hardware telemetry and package update status terminal user interface (TUI) dashboard written in Rust.

## Tech Stack
- **Language:** Rust
- **Libraries:**
  - `ratatui` (TUI framework)
  - `crossterm` (Terminal drawing and raw mode support)
  - `serde`/`serde_json` (Data handling)

## Features
- **Categorized Tabs:**
  1. *System & Load:* Overall loads, uptime, host details.
  2. *CPU Core Loads:* Granular usage percentages for individual CPU cores.
  3. *Memory, ZRAM & Disk:* Memory allocation and disk usage telemetry.
  4. *GPUs:* Intel Arc and AMD GPU sensor stats (temps, fans, clock speeds).
  5. *Net & Sensors:* Up/down bandwidth speeds, hardware thermal sensors, and Wi-Fi capability.
- **Background System Updates:** Spawned check thread notifying pending updates for Pacman, Paru, and global NPM modules.
