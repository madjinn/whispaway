use anyhow::Result;
use clap::{Parser, Subcommand, ValueEnum};

mod tray;
mod helpers;
mod recording;
mod typing;
mod socket;
mod whisper_cpp;
mod faster_whisper;
mod config;

#[derive(Parser)]
#[command(name = "whisp-away")]
#[command(about = "Simple dictation tool using whisper.cpp or faster-whisper", long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Debug, Clone, ValueEnum)]
enum Backend {
    /// Use whisper.cpp backend
    #[value(name = "whisper-cpp", alias = "cpp")]
    WhisperCpp,
    /// Use faster-whisper backend
    #[value(name = "faster-whisper", alias = "faster")]
    FasterWhisper,
    /// Use the backend defined in the tray state
    #[value(name = "tray-defined", alias = "tray")]
    TrayDefined,
}

#[derive(Subcommand)]
enum Commands {
    /// Start recording audio
    Start {
        /// Backend to use for transcription
        #[arg(short, long, default_value = "tray")]
        backend: Backend,
    },
    
    /// Stop recording and transcribe
    Stop {
        /// Backend to use for transcription
        #[arg(short, long, default_value = "tray")]
        backend: Backend,
        
        /// Use whisper-rs bindings for fallback (default: true, whisper-cpp only)
        #[arg(long, default_value_t = true)]
        bindings: bool,
        
        /// Model to use for transcription (overrides WA_WHISPER_MODEL env var)
        #[arg(short, long)]
        model: Option<String>,
        
        /// Optional audio file to transcribe (instead of recorded audio)
        #[arg(short, long)]
        audio_file: Option<String>,
        
        /// Unix socket path for daemon communication
        #[arg(long)]
        socket_path: Option<String>,
        
        /// Path to whisper.cpp binary (for whisper-cpp backend)
        #[arg(long)]
        whisper_path: Option<String>,
        
        /// Output to clipboard instead of typing at cursor (overrides tray/env setting)
        #[arg(long)]
        clipboard: Option<bool>,
    },
    
    /// Run as a daemon server with model preloaded
    Daemon {
        /// Backend to use
        #[arg(short, long, default_value = "tray")]
        backend: Backend,
        
        /// Model to use (overrides WA_WHISPER_MODEL env var)
        #[arg(short, long)]
        model: Option<String>,
        
        /// Unix socket path for daemon communication
        #[arg(long)]
        socket_path: Option<String>,
    },
    
    /// Run system tray icon for daemon control
    Tray {
        /// Backend to monitor
        #[arg(short, long, default_value = "tray")]
        backend: Backend,
    },
}

/// Resolves the backend to use, handling TrayDefined case
/// Priority:
/// 1. Tray state file
/// 2. System config file (~/.config/whisp-away/config.json)
/// 3. WA_WHISPER_BACKEND env var
/// 4. Default to "faster-whisper"
fn resolve_backend(backend: &Backend) -> String {
    match backend {
        Backend::WhisperCpp => "whisper-cpp".to_string(),
        Backend::FasterWhisper => "faster-whisper".to_string(),
        Backend::TrayDefined => {
            // Priority 1: Tray state
            if let Some(state) = helpers::read_tray_state() {
                return state.backend;
            }
            
            // Priority 2: System config file
            if let Some(config) = config::read_config() {
                return config.default_backend;
            }
            
            // Priority 3: Environment variable
            // Priority 4: Default
            std::env::var("WA_WHISPER_BACKEND").unwrap_or_else(|_| "faster-whisper".to_string())
        }
    }
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        // New unified commands
        Commands::Start { backend } => {
            // Resolve backend if TrayDefined
            let resolved_backend = resolve_backend(&backend);
            
            match resolved_backend.as_str() {
                "whisper-cpp" => recording::start_recording("whisper-cpp"),
                "faster-whisper" => recording::start_recording("faster-whisper"),
                unknown => Err(anyhow::anyhow!("Unknown backend: {}", unknown)),
            }
        }
        
        Commands::Stop { backend, bindings, model, audio_file, socket_path, whisper_path, clipboard } => {
            // Resolve backend (handles TrayDefined case)
            let resolved_backend = resolve_backend(&backend);
            
            let socket_path = socket::resolve_socket_path(socket_path);
            let use_clipboard = helpers::resolve_use_clipboard(clipboard);
            
            match resolved_backend.as_str() {
                "whisper-cpp" => {
                    // Pass bindings flag to daemon client (will be used in fallback)
                    whisper_cpp::stop_and_transcribe_daemon(&socket_path, audio_file.as_deref(), model, bindings, whisper_path, use_clipboard)
                }
                "faster-whisper" => {
                    // faster-whisper doesn't use bindings flag
                    faster_whisper::stop_and_transcribe_daemon(&socket_path, use_clipboard)
                }
                _ => Err(anyhow::anyhow!("Unknown backend: {}", resolved_backend))
            }
        }
        
        Commands::Daemon { backend, model, socket_path } => {
            let resolved_backend = resolve_backend(&backend);
            let model = helpers::resolve_model(model);
            let socket_path = socket::resolve_socket_path(socket_path);
            
            match resolved_backend.as_str() {
                "whisper-cpp" => whisper_cpp::run_daemon(&model, &socket_path),
                "faster-whisper" => faster_whisper::run_daemon(&model, &socket_path),
                unknown => Err(anyhow::anyhow!("Unknown backend: {}", unknown)),
            }
        }
        
        Commands::Tray { backend } => {
            let daemon_type = resolve_backend(&backend);
            tokio::runtime::Runtime::new()?.block_on(tray::run_tray(daemon_type))
        }
    }
}