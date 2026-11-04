use anyhow::{Context, Result};
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::process::Command;
use crate::typing;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

const SOCKET_NAME: &str = "whisp-away-daemon.sock";

/// The daemon's socket for this user: `cli` (--socket-path), else
/// WA_WHISPER_SOCKET, else $XDG_RUNTIME_DIR/whisp-away-daemon.sock, else
/// a per-user path in the cache directory. Never a shared path such as
/// /tmp, where another local user could listen in the daemon's place.
pub fn resolve_socket_path(cli: Option<String>) -> String {
    if let Some(path) = cli.filter(|p| !p.is_empty()) {
        return path;
    }
    default_socket_path()
}

/// The socket path without a --socket-path flag (see `resolve_socket_path`).
pub fn default_socket_path() -> String {
    if let Ok(path) = std::env::var("WA_WHISPER_SOCKET") {
        if !path.is_empty() {
            return path;
        }
    }
    if let Ok(dir) = std::env::var("XDG_RUNTIME_DIR") {
        if !dir.is_empty() && Path::new(&dir).is_dir() {
            return PathBuf::from(dir).join(SOCKET_NAME).to_string_lossy().into_owned();
        }
    }
    let dir = dirs::cache_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".cache"))
        .join("whisp-away");
    let _ = std::fs::create_dir_all(&dir);
    let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
    dir.join(SOCKET_NAME).to_string_lossy().into_owned()
}

/// Removes a stale socket file, only if it is this user's own.
pub fn remove_own_socket(socket_path: &str) {
    if check_socket_owner(socket_path).is_ok() {
        let _ = std::fs::remove_file(socket_path);
    }
}

/// Refuses a socket that exists but belongs to another user: its answer
/// would be typed as the transcript. A missing path is fine.
pub fn check_socket_owner(socket_path: &str) -> Result<()> {
    match std::fs::symlink_metadata(socket_path) {
        Ok(meta) => {
            let uid = unsafe { libc::getuid() };
            if meta.uid() != uid {
                anyhow::bail!(
                    "Refusing daemon socket {}: owned by uid {}, not by this user (uid {})",
                    socket_path, meta.uid(), uid
                );
            }
            Ok(())
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e).with_context(|| format!("Failed to inspect daemon socket {}", socket_path)),
    }
}

/// Send a transcription request to the daemon via Unix socket
pub fn send_transcription_request(
    socket_path: &str,
    audio_file: &str,
    backend_name: &str,
    use_clipboard: bool,
) -> Result<()> {
    check_socket_owner(socket_path)?;
    match UnixStream::connect(socket_path) {
        Ok(mut stream) => {
            // Send request
            let request = format!(r#"{{"audio_path": "{}"}}"#, audio_file);
            stream.write_all(request.as_bytes())
                .context("Failed to send request to daemon")?;
            
            // Read response
            let mut response = String::new();
            stream.read_to_string(&mut response)
                .context("Failed to read response from daemon")?;
            
            // Check if transcription was successful
            let success = response.contains(r#""success":true"#) || response.contains(r#""success": true"#);
            
            if success {
                // Parse the transcribed text from JSON response
                let text = extract_text_from_response(&response);
                
                if let Some(transcribed_text) = text {
                    typing::output_text(transcribed_text.trim(), use_clipboard, &format!("{} daemon", backend_name))?;
                } else {
                    Command::new("notify-send")
                        .args(&[
                            "Voice Input",
                            &format!("⚠️ Could not parse response\nBackend: {}", backend_name),
                            "-t", "2000",
                            "-h", "string:x-canonical-private-synchronous:voice"
                        ])
                        .spawn()?;
                }
            } else {
                Command::new("notify-send")
                    .args(&[
                        "Voice Input",
                        &format!("❌ Transcription failed\nBackend: {}", backend_name),
                        "-t", "2000",
                        "-h", "string:x-canonical-private-synchronous:voice"
                    ])
                    .spawn()?;
            }
            
            Ok(())
        }
        Err(e) => {
            // Return the error so the caller can handle fallback logic
            Err(anyhow::anyhow!("Failed to connect to daemon: {}", e))
        }
    }
}

/// Extract the "text" field value from a JSON response string
fn extract_text_from_response(response: &str) -> Option<String> {
    if let Some(text_start_idx) = response.find(r#""text":"#) {
        let after_text = &response[text_start_idx + 7..];
        let content_start = after_text.trim_start();
        
        if content_start.starts_with('"') {
            let text_content = &content_start[1..];
            if let Some(end_quote) = text_content.find('"') {
                Some(text_content[..end_quote].to_string())
            } else {
                None
            }
        } else {
            None
        }
    } else {
        None
    }
}
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_path_prefers_flag_then_env_then_runtime_dir() {
        let runtime = std::env::temp_dir().join(format!("wa-runtime-{}", std::process::id()));
        std::fs::create_dir_all(&runtime).unwrap();
        std::env::set_var("XDG_RUNTIME_DIR", &runtime);
        std::env::remove_var("WA_WHISPER_SOCKET");
        assert_eq!(default_socket_path(), runtime.join("whisp-away-daemon.sock").to_string_lossy());
        std::env::set_var("WA_WHISPER_SOCKET", "/run/custom.sock");
        assert_eq!(default_socket_path(), "/run/custom.sock");
        assert_eq!(resolve_socket_path(Some("/run/flag.sock".into())), "/run/flag.sock");
        assert_eq!(resolve_socket_path(None), "/run/custom.sock");
        std::env::remove_var("WA_WHISPER_SOCKET");
        assert!(!default_socket_path().starts_with("/tmp/whisp-away-daemon"));
        let _ = std::fs::remove_dir_all(&runtime);
    }

    #[test]
    fn refuses_a_socket_owned_by_another_user() {
        // A missing path and our own file are fine.
        let own = std::env::temp_dir().join(format!("wa-own-{}.sock", std::process::id()));
        assert!(check_socket_owner(own.to_str().unwrap()).is_ok());
        std::fs::write(&own, b"").unwrap();
        assert!(check_socket_owner(own.to_str().unwrap()).is_ok());
        let _ = std::fs::remove_file(&own);
        // "/" belongs to root: refused unless the tests run as root.
        if unsafe { libc::getuid() } != 0 {
            assert!(check_socket_owner("/").is_err());
        }
    }
}
