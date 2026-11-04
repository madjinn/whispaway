{ lib, rustPlatform ? null, pkg-config, dbus, whisper-cpp ? null, makeWrapper, python3, fetchFromGitHub, cudaPackages, cmake, libclang, git, stdenv, pulseaudio, wtype, wl-clipboard, libnotify, vulkan-headers, vulkan-loader, shaderc, openblas, patchelf, openvino, tbb, callPackage, curl, accelerationType ? "vulkan", addDriverRunpath ? null
# Build system selection
, useCrane ? false
, craneLib ? null
}:

assert useCrane -> craneLib != null;
assert !useCrane -> rustPlatform != null;

let
  pythonWithPackages = python3.withPackages (ps: with ps; [
    faster-whisper
    numpy
    pyaudio
  ]);
  
  # Select the appropriate whisper-cpp variant based on accelerationType
  whisper-cpp-vulkan = callPackage ./whisper-cpp/whisper-cpp-vulkan.nix {};
  whisper-cpp-cpu = callPackage ./whisper-cpp/whisper-cpp-cpu-only.nix {};
  whisper-cpp-openvino = callPackage ./whisper-cpp/whisper-cpp-openvino-fixed.nix {};
  
  whisper-cpp-final = if whisper-cpp != null then whisper-cpp else (
    if accelerationType == "vulkan" then whisper-cpp-vulkan
    else if accelerationType == "openvino" then whisper-cpp-openvino
    else if accelerationType == "cuda" then whisper-cpp-cpu  # CUDA uses CPU build with CUDA features
    else whisper-cpp-cpu
  );
  
  # Model download scripts
  model-download-scripts = callPackage ./scripts/model-download.nix {
    inherit curl;
  };

  # Parse Cargo.lock to extract git dependency info
  cargoLock = builtins.fromTOML (builtins.readFile ./Cargo.lock);
  whisperRsPkg = lib.findFirst (pkg: pkg.name or null == "whisper-rs") null cargoLock.package;
  whisperRsSource = whisperRsPkg.source or (throw "whisper-rs not found in Cargo.lock");
  whisperRsRev = builtins.head (builtins.match ".*#([a-f0-9]+)" whisperRsSource);

  # Load hash from external file (update with: nix run .#update-git-deps)
  gitDeps = import ./git-deps.nix;
  whisper-rs-hash = gitDeps."whisper-rs";

  # The actual builder - same for both crane and rustPlatform
  buildPackage = if useCrane then craneLib.buildPackage else rustPlatform.buildRustPackage;
  
  # Common attributes that work for both
  commonAttrs = rec {
    pname = "whisp-away";
    version = "0.1.0";
    
    src = ./.;
    
    # Enable features based on acceleration type
    buildFeatures = lib.optionals (accelerationType == "vulkan") [ "vulkan" ]
                  ++ lib.optionals (accelerationType == "openvino") [ "openvino" ]
                  ++ lib.optionals (accelerationType == "cuda") [ "cuda" ];
    
    nativeBuildInputs = [ 
      pkg-config 
      makeWrapper 
      cmake 
      libclang 
      git 
      stdenv.cc
      patchelf
      shaderc  # Provides glslc for Vulkan shader compilation
    ] ++ lib.optionals (accelerationType == "cuda") [
      cudaPackages.cudatoolkit
      addDriverRunpath
    ];
    
    buildInputs = [
      dbus 
    ] ++ lib.optionals (accelerationType == "vulkan") [
      vulkan-headers
      vulkan-loader
      shaderc
      openblas
    ] ++ lib.optionals (accelerationType == "openvino") [
      openvino
      tbb
    ] ++ lib.optionals (accelerationType == "cuda") [
      cudaPackages.cudatoolkit
      cudaPackages.cudnn
      cudaPackages.cuda_cudart
      cudaPackages.libcublas
      cudaPackages.cuda_cccl
    ];
    
    # Set LIBCLANG_PATH for bindgen used by whisper-rs
    LIBCLANG_PATH = "${libclang.lib}/lib";
    
    # Ensure C compiler is available
    CC = "${stdenv.cc}/bin/cc";
    CXX = "${stdenv.cc}/bin/c++";
    
    # Let whisper-rs build whisper.cpp with Vulkan support
    # We provide the necessary paths and flags for the build
    
    # OpenVINO paths for whisper-rs-sys build script (only when using OpenVINO)
    OpenVINO_DIR = lib.optionalString (accelerationType == "openvino") "${openvino}/runtime/cmake";
    OPENVINO_LIB_DIR = lib.optionalString (accelerationType == "openvino") "${openvino}/runtime/lib/intel64";
    
    # Set Vulkan paths for CMake to find them (only when using Vulkan)
    Vulkan_INCLUDE_DIR = lib.optionalString (accelerationType == "vulkan") "${vulkan-headers}/include";
    Vulkan_LIBRARY = lib.optionalString (accelerationType == "vulkan") "${vulkan-loader}/lib/libvulkan.so";
    Vulkan_GLSLC_EXECUTABLE = lib.optionalString (accelerationType == "vulkan") "${shaderc}/bin/glslc";
    VULKAN_SDK = lib.optionalString (accelerationType == "vulkan") "${vulkan-headers}";
    VK_ICD_FILENAMES = lib.optionalString (accelerationType == "vulkan") "/run/opengl-driver/share/vulkan/icd.d/intel_icd.x86_64.json";
    
    # CMAKE flags for whisper-rs's internal whisper.cpp build
    # These env vars will be passed through to the CMake build
    CMAKE_BUILD_TYPE = "Release";
    CMAKE_POSITION_INDEPENDENT_CODE = "ON";
    
    # Rust optimizations for maximum performance
    RUSTFLAGS = "-C target-cpu=native -C opt-level=3 -C lto=thin -C codegen-units=1";
    CARGO_PROFILE_RELEASE_LTO = "thin";
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "1";
    CARGO_PROFILE_RELEASE_OPT_LEVEL = "3";
    
    # Enable native CPU optimizations for GGML
    GGML_NATIVE = "ON";
    
    # Only set CUDA variables if CUDA acceleration is enabled
    CUDA_PATH = lib.optionalString (accelerationType == "cuda") "${cudaPackages.cudatoolkit}";
    CUDA_HOME = lib.optionalString (accelerationType == "cuda") "${cudaPackages.cudatoolkit}";
    CUDACXX = lib.optionalString (accelerationType == "cuda") "${cudaPackages.cudatoolkit}/bin/nvcc";
    CMAKE_CUDA_COMPILER = lib.optionalString (accelerationType == "cuda") "${cudaPackages.cudatoolkit}/bin/nvcc";
  
    # Add library paths for linking
    NIX_LDFLAGS = lib.optionalString (accelerationType == "openvino") 
      "-L${openvino}/runtime/lib/intel64 -L${tbb}/lib -lopenvino -lopenvino_c -ltbb"
      + lib.optionalString (accelerationType == "cuda")
      " -L${cudaPackages.cuda_cudart}/lib/stubs -L${cudaPackages.cudatoolkit}/lib/stubs";
    
    # Patch the build process
    postPatch = ''
      # Set environment for whisper-rs build
      ${lib.optionalString (accelerationType == "openvino") ''
        export OPENVINO_LIB_DIR="${openvino}/runtime/lib/intel64"
        export OpenVINO_DIR="${openvino}/runtime/cmake"
      ''}
      
      echo "Build configuration:"
      echo "  Acceleration Type: ${accelerationType}"
      echo "  Using pre-fetched whisper-rs"
      ${lib.optionalString (accelerationType == "openvino") ''
        echo "  OpenVINO_DIR=$OpenVINO_DIR"
        echo "  OPENVINO_LIB_DIR=$OPENVINO_LIB_DIR"
      ''}
    '';
    
    # Pre-build phase  
    preBuild = ''
      # Ensure environment variables are set for the cargo build
      
      ${lib.optionalString (accelerationType == "openvino") ''
        export OpenVINO_DIR="${openvino}/runtime/cmake"
        export OPENVINO_LIB_DIR="${openvino}/runtime/lib/intel64"
        # Force the openvino feature to be enabled
        export CARGO_FEATURE_OPENVINO=1
      ''}
      
      ${lib.optionalString (accelerationType == "vulkan") ''
        # Force the vulkan feature to be enabled
        export CARGO_FEATURE_VULKAN=1
      ''}
      
      # Debug: Show what environment variables are set
      echo "=== Build Environment ==="
      echo "Acceleration Type: ${accelerationType}"
      ${lib.optionalString (accelerationType == "openvino") ''
        echo "OpenVINO_DIR=$OpenVINO_DIR"
        echo "OPENVINO_LIB_DIR=$OPENVINO_LIB_DIR"
      ''}
      ${lib.optionalString (accelerationType == "vulkan") ''
        echo "Vulkan enabled - will build with Vulkan support"
        echo "Vulkan_INCLUDE_DIR=$Vulkan_INCLUDE_DIR"
        echo "Vulkan_LIBRARY=$Vulkan_LIBRARY"
      ''}
      echo "========================="
      
      # Set library paths for the build based on acceleration type
      ${lib.optionalString (accelerationType == "openvino") ''
        export LD_LIBRARY_PATH="${openvino}/runtime/lib/intel64:${tbb}/lib:$LD_LIBRARY_PATH"
        export LIBRARY_PATH="${openvino}/runtime/lib/intel64:${tbb}/lib:$LIBRARY_PATH"
      ''}
      
      ${lib.optionalString (accelerationType == "vulkan") ''
        export LD_LIBRARY_PATH="${vulkan-loader}/lib:${shaderc}/lib:$LD_LIBRARY_PATH"
        export LIBRARY_PATH="${vulkan-loader}/lib:${shaderc}/lib:$LIBRARY_PATH"
      ''}
    '';
  
    # Add runtime environment variables and helper script
    postInstall = ''
      echo "Built whisp-away with acceleration type: ${accelerationType}"
      
      # Install both Python scripts as RAW Python (not wrapped)
      # We'll call these with Python directly and inject environment from Rust
      install -Dm755 ${./src/faster_whisper/scripts/transcribe_faster.py} $out/share/whisp-away/transcribe_faster.py
      install -Dm755 ${./src/faster_whisper/scripts/whisper_daemon.py} $out/share/whisp-away/whisper_daemon.py
      
      # First, patch the binary with the required RPATH for libraries
      patchelf --set-rpath "${lib.makeLibraryPath ([
          stdenv.cc.cc.lib
        ] ++ lib.optionals (accelerationType == "openvino") [
          openvino
          tbb
        ] ++ lib.optionals (accelerationType == "vulkan") [
          vulkan-loader
          shaderc
          openblas
        ] ++ lib.optionals (accelerationType == "cuda") [
          cudaPackages.cudatoolkit.lib
          cudaPackages.cudatoolkit
          cudaPackages.libcublas
          cudaPackages.cudnn
        ]
      )}${lib.optionalString (accelerationType == "openvino") ":${openvino}/runtime/lib/intel64"}:/run/opengl-driver/lib:$(patchelf --print-rpath $out/bin/whisp-away)" \
        $out/bin/whisp-away
      
      # Add driver runpath for CUDA (resolves libcuda.so at runtime from NVIDIA driver)
      ${lib.optionalString (accelerationType == "cuda" && addDriverRunpath != null) ''
        addDriverRunpath $out/bin/whisp-away
      ''}
      
      # Wrap the Rust binary with necessary paths and whisper-cpp CLI
      # Detect which whisper backend is available
      WHISPER_BIN=""
      if [ -f "${whisper-cpp-final}/bin/whisper-cli-cpu" ]; then
        WHISPER_BIN="${whisper-cpp-final}/bin/whisper-cli-cpu"
      elif [ -f "${whisper-cpp-final}/bin/whisper-cli-vulkan" ]; then
        WHISPER_BIN="${whisper-cpp-final}/bin/whisper-cli-vulkan"
      elif [ -f "${whisper-cpp-final}/bin/whisper-cli-openvino" ]; then
        WHISPER_BIN="${whisper-cpp-final}/bin/whisper-cli-openvino"
      elif [ -f "${whisper-cpp-final}/bin/whisper-cpp-vulkan" ]; then
        WHISPER_BIN="${whisper-cpp-final}/bin/whisper-cpp-vulkan"
      elif [ -f "${whisper-cpp-final}/bin/whisper-cpp-cpu" ]; then
        WHISPER_BIN="${whisper-cpp-final}/bin/whisper-cpp-cpu"
      elif [ -f "${whisper-cpp-final}/bin/whisper-cpp-openvino" ]; then
        WHISPER_BIN="${whisper-cpp-final}/bin/whisper-cpp-openvino"
      elif [ -f "${whisper-cpp-final}/bin/main" ]; then
        WHISPER_BIN="${whisper-cpp-final}/bin/main"
      elif [ -f "${whisper-cpp-final}/bin/whisper-cli" ]; then
        WHISPER_BIN="${whisper-cpp-final}/bin/whisper-cli"
      fi
      
      # Debug output during build
      echo "Looking for whisper binary in: ${whisper-cpp-final}/bin/"
      ls -la "${whisper-cpp-final}/bin/" || echo "Directory does not exist"
      echo "Selected WHISPER_BIN: $WHISPER_BIN"
      
      wrapProgram $out/bin/whisp-away \
        --set WHISPER_CPP_PATH "$WHISPER_BIN" \
        --set FASTER_WHISPER_SCRIPT "$out/share/whisp-away/transcribe_faster.py" \
        --set FASTER_WHISPER_DAEMON_SCRIPT "$out/share/whisp-away/whisper_daemon.py" \
        --set FASTER_WHISPER_PYTHON "${pythonWithPackages}/bin/python3" \
        --set FASTER_WHISPER_PYTHONPATH "${pythonWithPackages}/${python3.sitePackages}" \
        --set WA_ACCELERATION_TYPE "${accelerationType}" \
        ${lib.optionalString (accelerationType == "cuda") ''--set CUDA_VISIBLE_DEVICES "0"''} \
        --prefix PATH : "${lib.makeBinPath [ pulseaudio wtype wl-clipboard libnotify pythonWithPackages ]}" \
        --prefix LD_LIBRARY_PATH : "${lib.makeLibraryPath (
          lib.optionals (accelerationType == "openvino") [ openvino tbb ]
          ++ lib.optionals (accelerationType == "vulkan") [ vulkan-loader shaderc openblas ]
        )}${lib.optionalString (accelerationType == "openvino") ":${openvino}/runtime/lib/intel64"}:/run/opengl-driver/lib" \
        --set-default DISPLAY ":0" \
        --set-default WAYLAND_DISPLAY "wayland-1"
      
      # Install model download scripts
      ${lib.optionalString (model-download-scripts ? download-whisper-model) ''
        echo "Installing download-whisper-model script..."
        install -Dm755 ${model-download-scripts.download-whisper-model}/bin/* $out/bin/
      ''}
      ${lib.optionalString (model-download-scripts ? list-whisper-models) ''
        echo "Installing list-whisper-models script..."
        install -Dm755 ${model-download-scripts.list-whisper-models}/bin/* $out/bin/
      ''}
  
      # Install desktop entry file for XDG autostart
      install -Dm644 ${./whisp-away.desktop} $out/share/applications/whisp-away.desktop
      
      # Update the Exec line to point to our wrapped binary
      substituteInPlace $out/share/applications/whisp-away.desktop \
        --replace "Exec=whisp-away" "Exec=$out/bin/whisp-away"
    '';
    
    meta = with lib; {
      description = "Rust-based voice input with whisper.cpp";
      license = licenses.gpl3Only;
      maintainers = [];
    };
  };
  
  # Crane-specific attributes
  craneAttrs = {
    cargoExtraArgs = lib.concatStringsSep " " (
      map (feature: "--features ${feature}") commonAttrs.buildFeatures
    );
    
    # Crane uses cargoVendorDir instead of cargoLock
    cargoVendorDir = craneLib.vendorCargoDeps {
      src = craneLib.cleanCargoSource ./.;
      cargoLock = ./Cargo.lock;
      outputHashes = {
        # Git dependencies need the hash of the unpacked git checkout
        # Use nix-prefetch-git to get this hash if it changes
        "whisper-rs-0.15.1" = whisper-rs-hash;
      };
    };
    
    # Override LTO settings for crane to avoid conflicts
    RUSTFLAGS = "-C target-cpu=native -C opt-level=3";
    CARGO_PROFILE_RELEASE_LTO = "false";
    CARGO_PROFILE_RELEASE_CODEGEN_UNITS = "16";
  };
  
  # RustPlatform-specific attributes  
  rustPlatformAttrs = {
    cargoLock = {
      lockFile = ./Cargo.lock;
      outputHashes = {
        "whisper-rs-0.15.1" = whisper-rs-hash;
      };
    };
  };
in
if useCrane then
  let
    # Build dependencies separately for caching
    # Remove postInstall from deps build since it expects the binary to exist
    depsAttrs = builtins.removeAttrs commonAttrs ["postInstall"];
    cargoArtifacts = craneLib.buildDepsOnly (depsAttrs // craneAttrs // {
      pname = "${commonAttrs.pname}-deps";
      doInstallCargoArtifacts = true;
    });
  in
  craneLib.buildPackage (commonAttrs // craneAttrs // {
    inherit cargoArtifacts;
  })
else
  buildPackage (commonAttrs // rustPlatformAttrs)