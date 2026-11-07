<h1>
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="assets/whispaway-logo-dark.svg">
    <img alt="WhispAway" src="assets/whispaway-logo-light.svg" width="360">
  </picture>
</h1>

<!-- BANNER: wording pending the Principal's OK via whispaway-a6; link: https://apps.apple.com/se/app/apple-store/id6818049570?pt=128223142&ct=github-linux&mt=8 · https://whispaway.app -->

Voice dictation for Linux using OpenAI's Whisper models. Type with your voice using local speech recognition - no cloud services required.

## Features

- **Flexible Output**: Instant typing at cursor or copy to clipboard
- **Dual Backends**: Choose between `whisper.cpp` or `faster-whisper`
- **Hardware Acceleration**: CUDA, Vulkan, OpenVINO, and CPU support
- **Model Preloading**: Daemon mode keeps models in memory for instant transcription
- **System Tray Control**: Start/stop daemon and toggle output mode
- **NixOS Integration**: First-class NixOS and Home Manager support

## Installation

### NixOS / Home Manager

```nix
{
  # With NixOS
  imports = [ whisp-away.nixosModules.nixos ];
  # With home-manager (recommended)
  imports = [ whisp-away.nixosModules.home-manager ];
  
  services.whisp-away = {
    enable = true;
    defaultModel = "base.en";         # Default model (changes apply immediately)
    defaultBackend = "whisper-cpp";   # Backend selection (changes apply immediately)
    accelerationType = "vulkan";      # or "cuda", "openvino", "cpu" - requires rebuild
    useClipboard = false;             # Output mode (changes apply immediately)
    useCrane = false;                 # Enable if you want faster rebuilds when developing
  };
}
```

**Note**: Most configuration changes (`defaultModel`, `defaultBackend`, `useClipboard`) take effect immediately after rebuild. Only `accelerationType` requires a full rebuild since it affects how the binary is compiled.

## Usage

### Keybinds (Recommended)

Configure your keybinds to enable push-to-talk:

For example in Hyprland config, push to talk and release to transcribe:
```conf
# section = the § key on Swedish keyboards (top-left, below Esc)
bind = ,section,exec, whisp-away start
bindr = ,section,exec, whisp-away stop
```

### System Tray (Recommended)

Improve transcription speed by preloading models.

Access from your `desktop apps`, or start from a terminal:

```bash
whisp-away tray                    # Uses default backend ($WA_WHISPER_BACKEND)
whisp-away tray -b faster-whisper  # Use faster-whisper backend
```

The tray icon lets you:
- **Left-click**: Start/stop daemon for preloaded models
- **Right-click**: Open menu to toggle output mode (clipboard/typing), check status, and switch backends

### Command Line

```bash
# One-shot recording and transcription
whisp-away start              # Start recording
whisp-away stop               # Stop and transcribe

# Specify model, backend, or output mode
whisp-away stop --model medium.en
whisp-away stop --backend faster-whisper
whisp-away stop --clipboard true     # Copy to clipboard instead of typing
```

## Models & Performance

| Model | Size | Speed | Quality | Use Case |
|-------|------|-------|---------|----------|
| **tiny.en** | 39 MB | Instant | Basic | Quick notes, testing |
| **base.en** | 74 MB | Fast | Good | Casual dictation |
| **small.en** | 244 MB | Moderate | Better | Daily use (recommended) |
| **medium.en** | 769 MB | Slow | Excellent | Professional transcription |
| **large-v3** | 1550 MB | Slowest | Best | Maximum accuracy, multilingual |

Models download automatically on first use, and are stored in `~/.cache/whisper-cpp/models/` (GGML models for whisper.cpp) and `~/.cache/faster-whisper/` (CTranslate2 models for faster-whisper).

For OpenVINO the GGML models have to be translated into the openVINO format (see docs in the whisper.cpp repo), this hasn't been automized yet.

## Hardware Acceleration

WhispAway supports multiple acceleration types:

| Type | Backend Support | Hardware |
|------|----------------|----------|
| **vulkan** | whisper.cpp | Most GPUs (AMD, NVIDIA, Intel) |
| **cuda** | Both backends | NVIDIA GPUs only |
| **openvino** | whisper.cpp | Intel GPUs and CPUs |
| **cpu** | Both backends | Any CPU (slow) |

**Note**: `faster-whisper` only supports CUDA and CPU. The `whisper.cpp` backend supports all acceleration types.

## Building from Source

### With Nix
```bash
nix build        # Builds with default settings
nix develop      # Enter development shell
```

### With Cargo
```bash
cargo build --release --features vulkan
```

## Configuration

### NixOS Module Options

```nix
services.whisp-away = {
  enable = true;
  defaultModel = "small.en";        # Default Whisper model
  defaultBackend = "whisper-cpp";   # Default backend (whisper-cpp or faster-whisper)
  accelerationType = "vulkan";      # GPU acceleration type (requires rebuild)
  useClipboard = false;             # Output mode (false = type, true = clipboard)
}
```

**Configuration takes effect immediately** after `nixos-rebuild` or `home-manager switch` - no logout required! The module writes to `~/.config/whisp-away/config.json` which is read by all whisp-away processes.

### Configuration Priority

Settings are resolved in this priority order (highest to lowest):

1. **Command-line arguments** (e.g., `--model medium.en`, `--clipboard true`)
2. **Tray state file** (`~/.config/whisp-away/state.json` - runtime changes via tray menu)
3. **Config file** (`~/.config/whisp-away/config.json` - managed by NixOS/home-manager)
4. **Environment variables** (`WA_WHISPER_MODEL`, `WA_WHISPER_BACKEND`, `WA_USE_CLIPBOARD`)
5. **Default values** (base.en, faster-whisper, clipboard=false)

This means:
- Command-line flags always win
- Tray menu changes override NixOS config temporarily
- NixOS module config provides system defaults
- Environment variables work for backwards compatibility

### Environment Variables (Legacy)

For manual configuration or non-NixOS systems:

- `WA_WHISPER_MODEL`: Default model (e.g., "small.en")
- `WA_WHISPER_BACKEND`: Default backend ("whisper-cpp" or "faster-whisper")
- `WA_USE_CLIPBOARD`: Output mode ("true" = clipboard, "false" = typing at cursor)

## Troubleshooting

**Tray icon doesn't appear?**
- Make sure you have a system tray (GNOME needs an extension)
- Check if the app is running: `ps aux | grep whisp-away`

**Transcription is slow?**
- Use a smaller model (tiny.en or base.en)
- Enable GPU acceleration if available
- The daemon pre-loads the model for faster response

**No text appears after recording?**
- Check the notification for errors
- For typing mode: 
  - Wayland: Verify `wtype` is installed
  - X11: Verify `xdotool` is installed (automatic fallback)
- For clipboard mode: Verify `wl-copy` (Wayland) or `xclip` (X11) is installed
- Try toggling output mode in the tray menu or use `--clipboard true/false`

## Project Status

This project is actively maintained and only tested on NixOS. Contributions are welcome!

## License

WhispAway for Linux is licensed under GPL-3.0-only from v1.0 (Copyright © 2025–2026 Madjinn Utveckling AB). Versions up to v0.4.0 (commit c1561e2) were MIT; contributions made under MIT keep their notice ([LICENSES/MIT.txt](LICENSES/MIT.txt)).

The code is free software; the name WhispAway and its logo are trademarks of Madjinn Utveckling AB and aren't covered by the licence — forks must use another name.

## Credits

- [OpenAI Whisper](https://github.com/openai/whisper) - Original speech recognition models
- [whisper.cpp](https://github.com/ggerganov/whisper.cpp) - C++ implementation
- [faster-whisper](https://github.com/guillaumekln/faster-whisper) - CTranslate2 optimized implementation
- [whisper-rs](https://github.com/tazz4843/whisper-rs) - Rust bindings