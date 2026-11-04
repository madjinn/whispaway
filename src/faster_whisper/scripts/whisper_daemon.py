#!/usr/bin/env python3
"""
Faster-whisper daemon server that keeps the model loaded in memory.
Listens on a Unix socket for transcription requests.
"""

import sys
import os
import socket
import json
import signal
import logging
from pathlib import Path
from faster_whisper import WhisperModel

def default_socket_path():
    """The same path as the Rust client: WA_WHISPER_SOCKET, else
    $XDG_RUNTIME_DIR/whisp-away-daemon.sock, else the user's cache dir."""
    path = os.environ.get("WA_WHISPER_SOCKET")
    if path:
        return path
    runtime = os.environ.get("XDG_RUNTIME_DIR")
    if runtime and os.path.isdir(runtime):
        return os.path.join(runtime, "whisp-away-daemon.sock")
    cache = os.environ.get("XDG_CACHE_HOME") or os.path.join(os.path.expanduser("~"), ".cache")
    directory = os.path.join(cache, "whisp-away")
    os.makedirs(directory, mode=0o700, exist_ok=True)
    return os.path.join(directory, "whisp-away-daemon.sock")

# Configure logging
logging.basicConfig(
    level=logging.INFO,
    format='%(asctime)s - %(levelname)s - %(message)s'
)
logger = logging.getLogger(__name__)

class WhisperDaemon:
    def __init__(self, model_name="medium.en", socket_path=None):
        self.model_name = model_name
        self.socket_path = socket_path or default_socket_path()
        self.model = None
        self.server_socket = None
        self.running = True
        
        # Set up signal handlers
        signal.signal(signal.SIGTERM, self.handle_signal)
        signal.signal(signal.SIGINT, self.handle_signal)
        
    def handle_signal(self, signum, frame):
        """Handle shutdown signals gracefully."""
        logger.info(f"Received signal {signum}, shutting down...")
        self.running = False
        if self.server_socket:
            self.server_socket.close()
        sys.exit(0)
        
    def load_model(self):
        """Load the Whisper model into memory."""
        logger.info(f"Loading model {self.model_name}...")
        
        # Determine device and compute type
        # Check WHISPER_DEVICE first, then fall back to CUDA_VISIBLE_DEVICES check
        device = os.environ.get("WHISPER_DEVICE", "cuda" if os.environ.get("CUDA_VISIBLE_DEVICES") else "cpu")
        compute_type = os.environ.get("WHISPER_COMPUTE", "int8_float16" if device == "cuda" else "int8")
        
        # Model cache directory
        cache_dir = os.path.expanduser("~/.cache/faster-whisper")
        os.makedirs(cache_dir, exist_ok=True)
        
        try:
            self.model = WhisperModel(
                self.model_name,
                device=device,
                compute_type=compute_type,
                download_root=cache_dir,
                num_workers=2  # Use multiple workers for better performance
            )
            logger.info(f"Model loaded successfully on {device}")
        except Exception as e:
            logger.error(f"Failed to load model: {e}")
            sys.exit(1)
            
    def transcribe(self, audio_path):
        """Transcribe an audio file."""
        try:
            segments, info = self.model.transcribe(
                audio_path,
                language="en",
                beam_size=5,
                best_of=5,
                temperature=0.0,
                vad_filter=True,
                vad_parameters=dict(
                    min_silence_duration_ms=300,  # Reduced for snappier detection
                    speech_pad_ms=100,  # Reduced padding
                    threshold=0.5
                )
            )
            
            # Collect text
            text = " ".join(segment.text.strip() for segment in segments)
            return {"success": True, "text": text}
            
        except Exception as e:
            logger.error(f"Transcription error: {e}")
            return {"success": False, "error": str(e)}
            
    def start_server(self):
        """Start the Unix socket server."""
        # Remove a stale socket of our own; never one of another user's
        if os.path.lexists(self.socket_path):
            owner = os.lstat(self.socket_path).st_uid
            if owner != os.getuid():
                raise RuntimeError(f"Refusing daemon socket {self.socket_path}: owned by uid {owner}, not by this user")
            os.unlink(self.socket_path)
            
        # Create Unix socket
        self.server_socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.server_socket.bind(self.socket_path)
        self.server_socket.listen(1)
        
        # Set socket permissions
        os.chmod(self.socket_path, 0o600)
        
        logger.info(f"Daemon listening on {self.socket_path}")
        
        while self.running:
            try:
                # Accept connections
                conn, _ = self.server_socket.accept()
                
                # Receive request
                data = conn.recv(4096).decode('utf-8')
                if not data:
                    conn.close()
                    continue
                    
                request = json.loads(data)
                audio_path = request.get('audio_path')
                
                if not audio_path or not os.path.exists(audio_path):
                    response = {"success": False, "error": "Invalid audio path"}
                else:
                    # Transcribe
                    response = self.transcribe(audio_path)
                    
                # Send response
                conn.send(json.dumps(response).encode('utf-8'))
                conn.close()
                
            except socket.error as e:
                if self.running:
                    logger.error(f"Socket error: {e}")
            except Exception as e:
                logger.error(f"Server error: {e}")
                
    def run(self):
        """Main daemon loop."""
        logger.info("Starting Whisper daemon...")
        
        # Load model
        self.load_model()
        
        # Start server
        self.start_server()

def main():
    # Get model from environment or use default
    model_name = os.environ.get("WA_WHISPER_MODEL", "medium.en")
    socket_path = default_socket_path()
    
    # Create and run daemon
    daemon = WhisperDaemon(model_name, socket_path)
    daemon.run()

if __name__ == "__main__":
    main()