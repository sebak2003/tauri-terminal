# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Tauri Terminal is a desktop app built with **Tauri 2.0** that combines a "My Day" task manager with an integrated terminal emulator. It uses a vanilla HTML/CSS/JS frontend (no bundler, no framework) with a Rust backend that manages PTY sessions via the `portable-pty` crate.

## Commands

```bash
# Development (starts Tauri dev server with hot reload)
npm run dev

# Production build
npm run build

# Run Rust checks only
cd src-tauri && cargo check

# Run Rust tests only
cd src-tauri && cargo test
```

**Prerequisites:** Rust toolchain, system dependencies for Tauri 2 on Linux (webkit2gtk, etc.), Node.js.

## Architecture

### Frontend (`src/`)

Single-page app in `src/index.html` — all HTML, CSS, and JS in one file. No build step, no bundler. Served directly by Tauri (`frontendDist: "../src"` in tauri.conf.json).

- **Task manager:** In-memory task list with add/delete/toggle, sorted by priority and completion. No persistence.
- **Terminal UI:** Uses vendored xterm.js (`src/vendor/`) with FitAddon. Supports multiple terminal tabs and horizontal split panes with drag-to-resize.
- **Tauri APIs** accessed via `window.__TAURI__` globals (`withGlobalTauri: true`), not via `@tauri-apps/api` imports.
- **Custom titlebar:** `decorations: false` in tauri.conf.json; window controls (close/minimize/maximize) and drag region implemented in HTML.

### Backend (`src-tauri/`)

- **`src/lib.rs`** — All backend logic. Manages PTY instances via `portable-pty`. Four Tauri commands:
  - `spawn_terminal` — Opens a PTY, spawns default shell, starts reader thread that emits `terminal-output` events
  - `write_terminal` — Forwards keystrokes from frontend to PTY
  - `resize_terminal` — Resizes PTY when terminal pane dimensions change
  - `close_terminal` — Drops PTY instance
- **State:** `TerminalState` holds a `Mutex<HashMap<u32, TerminalInstance>>` mapping terminal IDs to their PTY writer + master.
- **`src/main.rs`** — Entry point, just calls `app_lib::run()`.

### Frontend ↔ Backend Communication

- **Frontend → Backend:** `invoke('command_name', { args })` for Tauri commands
- **Backend → Frontend:** `app.emit("event-name", payload)` for terminal output and exit events
- **Events:** `terminal-output` (PTY data) and `terminal-exited` (process ended), listened via `listen()` on the frontend

### Capabilities (`src-tauri/capabilities/default.json`)

Permissions: `core:default`, window management (start-dragging, close, minimize, toggle-maximize). No shell, fs, or other plugin permissions enabled.
