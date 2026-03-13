use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::Serialize;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::Mutex;
use std::thread;
use tauri::{AppHandle, Emitter};

struct TerminalState {
    terminals: Mutex<HashMap<u32, TerminalInstance>>,
    next_id: Mutex<u32>,
}

struct TerminalInstance {
    writer: Box<dyn Write + Send>,
    master: Box<dyn portable_pty::MasterPty + Send>,
}

#[derive(Clone, Serialize)]
struct TerminalOutput {
    id: u32,
    data: String,
}

#[derive(Clone, Serialize)]
struct TerminalExited {
    id: u32,
}

#[tauri::command]
fn spawn_terminal(app: AppHandle, state: tauri::State<'_, TerminalState>) -> Result<u32, String> {
    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let mut cmd = CommandBuilder::new_default_prog();
    // Set TERM so colors and escape sequences work
    cmd.env("TERM", "xterm-256color");
    cmd.env("COLORTERM", "truecolor");

    let mut child = pair.slave.spawn_command(cmd).map_err(|e| e.to_string())?;

    // Drop slave — we only need the master side
    drop(pair.slave);

    let writer = pair.master.take_writer().map_err(|e| e.to_string())?;
    let mut reader = pair.master.try_clone_reader().map_err(|e| e.to_string())?;

    let mut id_lock = state.next_id.lock().unwrap();
    let id = *id_lock;
    *id_lock += 1;
    drop(id_lock);

    state.terminals.lock().unwrap().insert(
        id,
        TerminalInstance {
            writer,
            master: pair.master,
        },
    );

    // Spawn reader thread — reads PTY output and emits events to frontend
    let app_handle = app.clone();
    let term_id = id;
    thread::spawn(move || {
        let mut buf = [0u8; 4096];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    // Convert to string, replacing invalid UTF-8
                    let data = String::from_utf8_lossy(&buf[..n]).to_string();
                    let _ = app_handle.emit(
                        "terminal-output",
                        TerminalOutput {
                            id: term_id,
                            data,
                        },
                    );
                }
                Err(_) => break,
            }
        }
        let _ = app_handle.emit("terminal-exited", TerminalExited { id: term_id });
    });

    // Spawn thread to wait for child exit
    let app_handle2 = app.clone();
    let term_id2 = id;
    thread::spawn(move || {
        let _ = child.wait();
        let _ = app_handle2.emit("terminal-exited", TerminalExited { id: term_id2 });
    });

    Ok(id)
}

#[tauri::command]
fn write_terminal(
    data: String,
    id: u32,
    state: tauri::State<'_, TerminalState>,
) -> Result<(), String> {
    let mut terminals = state.terminals.lock().unwrap();
    if let Some(term) = terminals.get_mut(&id) {
        term.writer
            .write_all(data.as_bytes())
            .map_err(|e| e.to_string())?;
        term.writer.flush().map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn resize_terminal(
    id: u32,
    cols: u16,
    rows: u16,
    state: tauri::State<'_, TerminalState>,
) -> Result<(), String> {
    let terminals = state.terminals.lock().unwrap();
    if let Some(term) = terminals.get(&id) {
        term.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .map_err(|e| e.to_string())?;
    }
    Ok(())
}

#[tauri::command]
fn close_terminal(id: u32, state: tauri::State<'_, TerminalState>) -> Result<(), String> {
    let mut terminals = state.terminals.lock().unwrap();
    terminals.remove(&id);
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .manage(TerminalState {
            terminals: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
        })
        .invoke_handler(tauri::generate_handler![
            spawn_terminal,
            write_terminal,
            resize_terminal,
            close_terminal,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
