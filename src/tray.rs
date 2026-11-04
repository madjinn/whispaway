use anyhow::{Context, Result};
use ksni::{menu::StandardItem, MenuItem, Tray, TrayService};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::net::UnixStream;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use crate::helpers::{TrayState, write_tray_state};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DaemonStatus {
    running: bool,
    model: String,
    processing: bool,
    use_clipboard: bool,
}

impl Default for DaemonStatus {
    fn default() -> Self {
        // Use resolve functions to get the initial values (respects env vars)
        Self {
            running: false,
            model: crate::helpers::resolve_model(None),
            processing: false,
            use_clipboard: crate::helpers::resolve_use_clipboard(None),
        }
    }
}

#[derive(Debug)]
struct VoiceInputTray {
    status: Arc<Mutex<DaemonStatus>>,
    daemon_type: String, // "faster-whisper" or "whisper-cpp"
    daemon_process: Arc<Mutex<Option<Child>>>, // The actual daemon process
}

impl Drop for VoiceInputTray {
    fn drop(&mut self) {
        // Clean up daemon process when tray exits
        if let Err(e) = self.stop_daemon_process() {
            eprintln!("Failed to stop daemon on exit: {}", e);
        }
    }
}

impl VoiceInputTray {
    fn new(daemon_type: String) -> Self {
        let tray = Self {
            status: Arc::new(Mutex::new(DaemonStatus::default())),
            daemon_type,
            daemon_process: Arc::new(Mutex::new(None)),
        };
        
        // Save initial state
        if let Err(e) = tray.save_state() {
            eprintln!("Warning: Failed to save initial tray state: {}", e);
        }
        
        tray
    }
    
    fn save_state(&self) -> Result<()> {
        let status = self.status.lock().unwrap();
        let state = TrayState {
            model: status.model.clone(),
            backend: self.daemon_type.clone(),
            use_clipboard: status.use_clipboard,
        };
        drop(status);
        write_tray_state(&state)
    }
    
    fn start_daemon_process(&self) -> Result<()> {
        // First, clean up any orphaned processes from previous runs
        if self.daemon_type == "faster-whisper" {
            // Kill any existing Python daemon processes
            let _ = Command::new("pkill")
                .args(&["-f", "whisper_daemon.py"])
                .output();
            
            // Remove stale socket file if it exists  
            crate::socket::remove_own_socket(&crate::socket::default_socket_path());
        } else {
            // Remove stale socket file (same path for both backends now)
            crate::socket::remove_own_socket(&crate::socket::default_socket_path());
        }
        
        // Check if already running
        if let Ok(mut process_guard) = self.daemon_process.lock() {
            if let Some(ref mut child) = *process_guard {
                // Check if process is still running
                match child.try_wait() {
                    Ok(Some(_)) => {
                        // Process has exited, we can start a new one
                        *process_guard = None;
                    }
                    Ok(None) => {
                        // Still running
                        return Ok(());
                    }
                    Err(_) => {
                        *process_guard = None;
                    }
                }
            }
            
            // Get configuration from current state
            let model = {
                let status = self.status.lock().unwrap();
                status.model.clone()
            };
            let socket_path = crate::socket::default_socket_path();
            let home = std::env::var("HOME").unwrap_or_default();
            
            // Get the path to our own binary
            let binary_path = std::env::current_exe()
                .context("Failed to get current executable path")?;
            
            // Build the daemon command
            let mut cmd = Command::new(&binary_path);
            cmd.arg("daemon")
               .arg("--backend")
               .arg(&self.daemon_type)
               .arg("--model")
               .arg(&model);
            
            // The same socket path for both backends
            cmd.arg("--socket-path")
               .arg(&socket_path);
            cmd.env("WA_WHISPER_SOCKET", &socket_path);
            
            if self.daemon_type == "faster-whisper" {
                
                // Device and compute type for faster-whisper
                if std::env::var("CUDA_VISIBLE_DEVICES").is_ok() {
                    cmd.env("WHISPER_DEVICE", "cuda");
                    cmd.env("WHISPER_COMPUTE", "float16");
                } else {
                    cmd.env("WHISPER_DEVICE", "cpu");
                    cmd.env("WHISPER_COMPUTE", "int8");
                }
            } else {
                // Whisper.cpp specific - set model path
                let model_path = format!("{}/.cache/whisper-cpp/models/ggml-{}.bin", home, model);
                
                // Check if model exists, if not try to download it
                if !std::path::Path::new(&model_path).exists() {
                    println!("Model {} not found, attempting to download...", model);
                    
                    // Try to run download-whisper-model if available
                    let download_result = Command::new("download-whisper-model")
                        .arg(&model)
                        .output();
                    
                    match download_result {
                        Ok(output) if output.status.success() => {
                            println!("Model downloaded successfully");
                        }
                        _ => {
                            // Send notification about missing model
                            let _ = Command::new("notify-send")
                                .args(&[
                                    "Voice Input",
                                    &format!("⚠️ Model {} not found. Please download it manually:\ndownload-whisper-model {}", model, model),
                                    "-t", "10000",
                                    "-u", "critical"
                                ])
                                .spawn();
                            
                            eprintln!("Warning: Model {} not found and couldn't download", model);
                            // Continue anyway - daemon will fail if model is really needed
                        }
                    }
                }
                
                cmd.env("WHISPER_CPP_MODEL_PATH", &model_path);
            }
            
            // Common environment variables
            cmd.env("HOME", &home);
            cmd.env("WA_WHISPER_MODEL", &model);
            
            // Pass through important environment variables from parent
            for (key, value) in std::env::vars() {
                match key.as_str() {
                    // GPU/CUDA related
                    "CUDA_VISIBLE_DEVICES" | "CUDA_HOME" | "CUDA_PATH" |
                    // Library paths
                    "LD_LIBRARY_PATH" | "LIBRARY_PATH" |
                    // Python paths (for faster-whisper)
                    "FASTER_WHISPER_PYTHON" | "FASTER_WHISPER_PYTHONPATH" |
                    "FASTER_WHISPER_SCRIPT" | "FASTER_WHISPER_DAEMON_SCRIPT" |
                    // Whisper.cpp paths
                    "WHISPER_CPP_PATH" |
                    // Display (might be needed for some operations)
                    "DISPLAY" | "WAYLAND_DISPLAY" | "XDG_RUNTIME_DIR" => {
                        cmd.env(key, value);
                    }
                    _ => {}
                }
            }
            
            // Ensure cache directories exist
            let cache_base = format!("{}/.cache", home);
            std::fs::create_dir_all(format!("{}/whisp-away", cache_base)).ok();
            std::fs::create_dir_all(format!("{}/faster-whisper", cache_base)).ok();
            std::fs::create_dir_all(format!("{}/whisper-cpp/models", cache_base)).ok();
            
            // Redirect output to files for debugging
            let log_dir = format!("{}/whisp-away", cache_base);
            std::fs::create_dir_all(&log_dir).ok();
            
            let stdout_file = std::fs::File::create(format!("{}/daemon-{}.log", log_dir, self.daemon_type)).ok();
            let stderr_file = std::fs::File::create(format!("{}/daemon-{}.err", log_dir, self.daemon_type)).ok();
            
            if let Some(stdout) = stdout_file {
                cmd.stdout(Stdio::from(stdout));
            } else {
                cmd.stdout(Stdio::null());
            }
            
            if let Some(stderr) = stderr_file {
                cmd.stderr(Stdio::from(stderr));
            } else {
                cmd.stderr(Stdio::null());
            }
            
            // Spawn the daemon process in its own process group
            // This allows us to kill the entire group later
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);  // Create new process group
            
            let child = cmd.spawn()
                .context("Failed to spawn daemon process")?;
            
            *process_guard = Some(child);
            
            // Give the daemon a moment to start
            std::thread::sleep(Duration::from_secs(2));
            
            // Update status
            if let Ok(mut status) = self.status.lock() {
                status.running = true;
                status.model = model;
            }
            
            // Send notification
            let acceleration = crate::helpers::get_acceleration_type();
            let _ = Command::new("notify-send")
                .args(&[
                    "Voice Input",
                    &format!("✅ {} daemon started ({})", self.daemon_type, acceleration),
                    "-t", "3000",
                ])
                .spawn();
            
            Ok(())
        } else {
            Err(anyhow::anyhow!("Failed to acquire process lock"))
        }
    }
    
    fn stop_daemon_process(&self) -> Result<()> {
        if let Ok(mut process_guard) = self.daemon_process.lock() {
            if let Some(ref mut child) = *process_guard {
                let pid = child.id() as i32;
                
                // For faster-whisper, we need to be more aggressive about cleanup
                // because Python processes with GPU resources can be stubborn
                if self.daemon_type == "faster-whisper" {
                    // First, try to find and kill any Python processes that might be the actual daemon
                    // The daemon script name would be in the process list
                    let _ = Command::new("pkill")
                        .args(&["-f", "whisper_daemon.py"])
                        .output();
                    
                    // Also kill any process with the daemon socket in its command line
                    let _ = Command::new("pkill")
                        .args(&["-f", &crate::socket::default_socket_path()])
                        .output();
                }
                
                // Kill the entire process group (negative PID kills the group)
                unsafe {
                    // First try SIGTERM to the process group
                    libc::kill(-pid, libc::SIGTERM);
                }
                
                // Give it a moment to shut down gracefully
                std::thread::sleep(Duration::from_secs(1));
                
                // Check if the main process is still running
                match child.try_wait() {
                    Ok(None) => {
                        // Still running, force kill the process group
                        unsafe {
                            libc::kill(-pid, libc::SIGKILL);
                        }
                        
                        // Also force kill the direct child
                        child.kill().ok();
                        child.wait().ok();
                        
                        // For faster-whisper, do one more aggressive cleanup
                        if self.daemon_type == "faster-whisper" {
                            std::thread::sleep(Duration::from_millis(200));
                            // Force kill any remaining Python daemon processes
                            let _ = Command::new("pkill")
                                .args(&["-9", "-f", "whisper_daemon.py"])
                                .output();
                        }
                    }
                    _ => {
                        // Process already exited, but for faster-whisper still check for orphans
                        if self.daemon_type == "faster-whisper" {
                            // Clean up any orphaned Python processes
                            let _ = Command::new("pkill")
                                .args(&["-f", "whisper_daemon.py"])
                                .output();
                        }
                    }
                }
                
                // Clean up the socket file if it exists
                crate::socket::remove_own_socket(&crate::socket::default_socket_path());
                
                *process_guard = None;
                
                // Update status
                if let Ok(mut status) = self.status.lock() {
                    status.running = false;
                    status.processing = false;
                }
                
                // Send notification
                let _ = Command::new("notify-send")
                    .args(&[
                        "Voice Input",
                        &format!("⏹️ {} daemon stopped", self.daemon_type),
                        "-t", "3000",
                    ])
                    .spawn();
                
                Ok(())
            } else {
                Ok(()) // No process to stop
            }
        } else {
            Err(anyhow::anyhow!("Failed to acquire process lock"))
        }
    }
    
    fn check_daemon_process_status(&self) -> bool {
        if let Ok(mut process_guard) = self.daemon_process.lock() {
            if let Some(ref mut child) = *process_guard {
                // Check if process is still running
                match child.try_wait() {
                    Ok(None) => true,  // Still running
                    _ => {
                        // Process has exited
                        *process_guard = None;
                        false
                    }
                }
            } else {
                false
            }
        } else {
            false
        }
    }

    async fn check_daemon_status(&self) -> Result<bool> {
        let socket_path = match self.daemon_type.as_str() {
            "faster-whisper" | "whisper-cpp" => crate::socket::default_socket_path(),
            _ => return Ok(false),
        };

        if !Path::new(&socket_path).exists() || crate::socket::check_socket_owner(&socket_path).is_err() {
            return Ok(false);
        }

        // Try to connect to the daemon
        match UnixStream::connect(&socket_path).await {
            Ok(mut stream) => {
                // Send a status request
                let request = r#"{"command": "status"}"#;
                stream.write_all(request.as_bytes()).await?;
                
                // Try to read response
                let mut buffer = vec![0; 1024];
                match tokio::time::timeout(
                    Duration::from_secs(1),
                    stream.read(&mut buffer)
                ).await {
                    Ok(Ok(n)) if n > 0 => Ok(true),
                    _ => Ok(false),
                }
            }
            Err(_) => Ok(false),
        }
    }

    fn start_daemon(&self) -> Result<()> {
        self.start_daemon_process()
    }

    fn stop_daemon(&self) -> Result<()> {
        self.stop_daemon_process()
    }

    fn get_icon_name(&self) -> String {
        let status = self.status.lock().unwrap();
        if !status.running {
            "microphone-disabled-symbolic"
        } else if status.processing {
            "microphone-sensitivity-high-symbolic"
        } else {
            "microphone-sensitivity-medium-symbolic"
        }.to_string()
    }

    fn get_tooltip(&self) -> String {
        let status = self.status.lock().unwrap();
        if !status.running {
            format!("Voice Input ({}) - Stopped\nLeft-click to start", self.daemon_type)
        } else if status.processing {
            format!("Voice Input ({}) - Processing...", self.daemon_type)
        } else {
            format!(
                "Voice Input ({}) - Ready\nModel: {}\nLeft-click to stop",
                self.daemon_type, status.model
            )
        }
    }
}

impl Tray for VoiceInputTray {
    fn id(&self) -> String {
        format!("voice-input-{}", self.daemon_type)
    }

    fn title(&self) -> String {
        "Voice Input".to_string()
    }

    fn icon_name(&self) -> String {
        self.get_icon_name()
    }

    fn tool_tip(&self) -> ksni::ToolTip {
        ksni::ToolTip {
            title: self.get_tooltip(),
            ..Default::default()
        }
    }
    
    fn activate(&mut self, _x: i32, _y: i32) {
        // Left-click toggles daemon start/stop
        let is_running = {
            let status = self.status.lock().unwrap();
            status.running
        };
        
        if is_running {
            // Stop the daemon
            match self.stop_daemon() {
                Ok(_) => {
                    // Only update status on success
                    if let Ok(mut status) = self.status.lock() {
                        status.running = false;
                        status.processing = false;
                    }
                }
                Err(e) => {
                    eprintln!("Failed to stop daemon: {}", e);
                }
            }
        } else {
            // Start the daemon
            match self.start_daemon() {
                Ok(_) => {
                    // Only update status on success
                    if let Ok(mut status) = self.status.lock() {
                        status.running = true;
                    }
                }
                Err(e) => {
                    eprintln!("Failed to start daemon: {}", e);
                }
            }
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let status = self.status.lock().unwrap();
        let is_running = status.running;
        let model = status.model.clone();
        drop(status);

        let mut items = vec![];

        // Status indicator
        items.push(MenuItem::Standard(StandardItem {
            label: if is_running {
                "Status: ✅ Running".to_string()
            } else {
                "Status: ⏸️  Stopped".to_string()
            },
            enabled: false,
            ..Default::default()
        }));
        
        // Backend/daemon type indicator
        let daemon_display = if self.daemon_type == "faster-whisper" {
            "Faster Whisper"
        } else {
            "Whisper.cpp"
        };
        items.push(MenuItem::Standard(StandardItem {
            label: format!("Backend: {}", daemon_display),
            enabled: false,
            ..Default::default()
        }));
        
        // Acceleration type indicator
        let acceleration = crate::helpers::get_acceleration_type();
        items.push(MenuItem::Standard(StandardItem {
            label: format!("Acceleration: {}", acceleration.to_uppercase()),
            enabled: false,
            ..Default::default()
        }));
        
        // Clipboard output toggle
        let current_clipboard = {
            let status = self.status.lock().unwrap();
            status.use_clipboard
        };
        items.push(MenuItem::Standard(StandardItem {
            label: if current_clipboard {
                "Output: ✓ Clipboard".to_string()
            } else {
                "Output: ✓ Type at Cursor".to_string()
            },
            activate: Box::new(|tray: &mut Self| {
                if let Ok(mut status) = tray.status.lock() {
                    status.use_clipboard = !status.use_clipboard;
                    let new_mode = status.use_clipboard;
                    drop(status);
                    
                    // Save state
                    if let Err(e) = tray.save_state() {
                        eprintln!("Warning: Failed to save clipboard state: {}", e);
                    }
                    
                    // Show notification
                    let mode_text = if new_mode { "Clipboard" } else { "Type at Cursor" };
                    let _ = Command::new("notify-send")
                        .args(&[
                            "Voice Input",
                            &format!("Output mode: {}", mode_text),
                            "-t", "2000",
                        ])
                        .spawn();
                }
            }),
            ..Default::default()
        }));

        items.push(MenuItem::Separator);

        // Start/Stop control
        if is_running {
            items.push(MenuItem::Standard(StandardItem {
                label: "Stop Daemon".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.stop_daemon();
                    if let Ok(mut status) = tray.status.lock() {
                        status.running = false;
                        status.processing = false;
                    }
                }),
                ..Default::default()
            }));
        } else {
            items.push(MenuItem::Standard(StandardItem {
                label: "Start Daemon".to_string(),
                activate: Box::new(|tray: &mut Self| {
                    let _ = tray.start_daemon();
                    if let Ok(mut status) = tray.status.lock() {
                        status.running = true;
                    }
                }),
                ..Default::default()
            }));
        }

        // Model selection submenu
        items.push(MenuItem::Separator);
        items.push(MenuItem::Standard(StandardItem {
            label: format!("Model: {}", model),
            enabled: false,
            ..Default::default()
        }));


        items.push(MenuItem::Separator);

        // Switch daemon type
        let other_daemon = if self.daemon_type == "faster-whisper" {
            "whisper-cpp"
        } else {
            "faster-whisper"
        };
        
        let other_daemon_display = if self.daemon_type == "faster-whisper" {
            "Whisper.cpp"
        } else {
            "Faster Whisper"
        };
        
        let other_daemon_clone = other_daemon.to_string();
        items.push(MenuItem::Standard(StandardItem {
            label: format!("Switch to {}", other_daemon_display),
            activate: Box::new(move |tray: &mut Self| {
                // Stop current daemon if running
                let was_running = {
                    let status = tray.status.lock().unwrap();
                    status.running
                };
                
                if was_running {
                    match tray.stop_daemon() {
                        Ok(_) => {
                            if let Ok(mut status) = tray.status.lock() {
                                status.running = false;
                                status.processing = false;
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to stop {} for switch: {}", tray.daemon_type, e);
                            // Don't switch if we can't stop the current daemon
                            return;
                        }
                    }
                }
                
                // Switch daemon type
                tray.daemon_type = other_daemon_clone.clone();
                
                // Save new backend state
                if let Err(e) = tray.save_state() {
                    eprintln!("Warning: Failed to save tray state after backend switch: {}", e);
                }
                
                // Start the new daemon
                match tray.start_daemon() {
                    Ok(_) => {
                        if let Ok(mut status) = tray.status.lock() {
                            status.running = true;
                        }
                    }
                    Err(e) => {
                        eprintln!("Failed to start {}: {}", tray.daemon_type, e);
                    }
                }
            }),
            ..Default::default()
        }));

        items.push(MenuItem::Separator);

        // Quit
        items.push(MenuItem::Standard(StandardItem {
            label: "Quit".to_string(),
            activate: Box::new(|_tray: &mut Self| {
                std::process::exit(0);
            }),
            ..Default::default()
        }));

        items
    }
}

pub async fn run_tray(daemon_type: String) -> Result<()> {
    let tray = VoiceInputTray::new(daemon_type.clone());
    
    // DISABLED: Background status checker causes issues when switching daemon types
    // The checker doesn't know about daemon type changes and checks the wrong service
    // TODO: Fix this by making daemon_type mutable and shared
    
    // For now, we rely on manual status updates when starting/stopping daemons

    // Create and run the tray service
    let service = TrayService::new(tray);
    service.run();

    Ok(())
}