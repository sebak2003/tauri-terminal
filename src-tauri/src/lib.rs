use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde::{Deserialize, Serialize};
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

#[derive(Clone, Serialize, Deserialize)]
struct ProjectInfo {
    path: String,
    name: String,
}

#[tauri::command]
fn spawn_terminal(
    app: AppHandle,
    state: tauri::State<'_, TerminalState>,
    cwd: Option<String>,
) -> Result<u32, String> {
    let pty_system = native_pty_system();

    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| e.to_string())?;

    let mut cmd = match &cwd {
        Some(dir) => {
            if cfg!(target_os = "windows") {
                let mut c = CommandBuilder::new("wsl.exe");
                c.args(["-d", "Ubuntu", "--cd", dir.as_str()]);
                c
            } else {
                let mut c = CommandBuilder::new_default_prog();
                c.cwd(dir);
                c
            }
        }
        None => CommandBuilder::new_default_prog(),
    };

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

#[tauri::command]
fn scan_wsl_dirs(path: String) -> Result<Vec<ProjectInfo>, String> {
    let output = if cfg!(target_os = "windows") {
        std::process::Command::new("wsl.exe")
            .args([
                "-d",
                "Ubuntu",
                "-e",
                "find",
                &path,
                "-maxdepth",
                "1",
                "-mindepth",
                "1",
                "-type",
                "d",
            ])
            .output()
            .map_err(|e| e.to_string())?
    } else {
        std::process::Command::new("find")
            .args([&path, "-maxdepth", "1", "-mindepth", "1", "-type", "d"])
            .output()
            .map_err(|e| e.to_string())?
    };

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        return Err(format!("find command failed: {}", stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let mut projects: Vec<ProjectInfo> = stdout
        .lines()
        .filter(|line| !line.trim().is_empty())
        .filter_map(|line| {
            let trimmed = line.trim();
            let name = std::path::Path::new(trimmed)
                .file_name()?
                .to_str()?
                .to_string();
            Some(ProjectInfo {
                path: trimmed.to_string(),
                name,
            })
        })
        .collect();

    projects.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

    Ok(projects)
}

fn get_config_path() -> Result<std::path::PathBuf, String> {
    let config_dir = if cfg!(target_os = "windows") {
        let appdata = std::env::var("APPDATA").map_err(|_| "APPDATA env var not found")?;
        std::path::PathBuf::from(appdata)
    } else {
        let home = std::env::var("HOME").map_err(|_| "HOME env var not found")?;
        std::path::PathBuf::from(home).join(".config")
    };
    Ok(config_dir.join("tauri-terminal").join("projects.json"))
}

#[tauri::command]
fn load_projects() -> Result<Vec<ProjectInfo>, String> {
    let path = get_config_path()?;
    if !path.exists() {
        return Ok(Vec::new());
    }
    let contents = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
    let projects: Vec<ProjectInfo> = serde_json::from_str(&contents).map_err(|e| e.to_string())?;
    Ok(projects)
}

#[tauri::command]
fn save_projects(projects: Vec<ProjectInfo>) -> Result<(), String> {
    let path = get_config_path()?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let json = serde_json::to_string_pretty(&projects).map_err(|e| e.to_string())?;
    std::fs::write(&path, json).map_err(|e| e.to_string())?;
    Ok(())
}

// ===== Git helpers =====

fn run_git_command(path: &str, args: &[&str]) -> Result<String, String> {
    let output = if cfg!(target_os = "windows") {
        let mut cmd_args: Vec<&str> = vec!["-d", "Ubuntu", "-e", "git", "-C", path];
        cmd_args.extend_from_slice(args);
        std::process::Command::new("wsl.exe")
            .args(&cmd_args)
            .output()
            .map_err(|e| e.to_string())?
    } else {
        let mut cmd_args: Vec<&str> = vec!["-C", path];
        cmd_args.extend_from_slice(args);
        std::process::Command::new("git")
            .args(&cmd_args)
            .output()
            .map_err(|e| e.to_string())?
    };

    if !output.status.success() {
        return Err(String::from_utf8_lossy(&output.stderr).trim().to_string());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[derive(Clone, Serialize)]
struct GitStatus {
    is_repo: bool,
    branch: String,
    changed_count: u32,
    ahead: u32,
}

#[tauri::command]
fn get_git_status(path: String) -> GitStatus {
    let branch = match run_git_command(&path, &["branch", "--show-current"]) {
        Ok(b) => b,
        Err(_) => {
            return GitStatus {
                is_repo: false,
                branch: String::new(),
                changed_count: 0,
                ahead: 0,
            }
        }
    };

    let changed_count = run_git_command(&path, &["status", "--porcelain"])
        .map(|s| s.lines().filter(|l| !l.trim().is_empty()).count() as u32)
        .unwrap_or(0);

    let ahead = run_git_command(&path, &["rev-list", "--count", "@{u}..HEAD"])
        .ok()
        .and_then(|s| s.parse::<u32>().ok())
        .unwrap_or(0);

    GitStatus {
        is_repo: true,
        branch,
        changed_count,
        ahead,
    }
}

#[tauri::command]
fn git_add_and_commit(path: String, message: String) -> Result<(), String> {
    run_git_command(&path, &["add", "-A"])?;
    run_git_command(&path, &["commit", "-m", &message])?;
    Ok(())
}

#[tauri::command]
fn git_push(path: String) -> Result<(), String> {
    run_git_command(&path, &["push"])?;
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
            scan_wsl_dirs,
            load_projects,
            save_projects,
            get_git_status,
            git_add_and_commit,
            git_push,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
