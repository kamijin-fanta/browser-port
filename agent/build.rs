fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    configure_windows_executable_icon();
    configure_windows_common_controls_manifest();
    configure_windows_ndi_delay_load();
    build_windows_spout();
    build_macos_syphon();
}

fn configure_windows_executable_icon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let icon_rel = std::path::PathBuf::from("..").join("icons").join("trimed.ico");
    println!("cargo:rerun-if-changed={}", icon_rel.display());

    #[cfg(windows)]
    {
        let icon_path = std::env::current_dir()
            .map(|cwd| cwd.join(&icon_rel))
            .unwrap_or(icon_rel);
        let mut res = winres::WindowsResource::new();
        res.set_icon(icon_path.to_string_lossy().as_ref());
        if let Err(err) = res.compile() {
            panic!("failed to embed Windows icon resource: {err}");
        }
    }
}

fn configure_windows_common_controls_manifest() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    let manifest_rel = std::path::PathBuf::from("assets").join("windows-common-controls.manifest");
    println!("cargo:rerun-if-changed={}", manifest_rel.display());

    let manifest_path = std::env::current_dir()
        .map(|cwd| cwd.join(&manifest_rel))
        .unwrap_or(manifest_rel);

    // Some linked native components import newer comctl32 exports by ordinal.
    // Embedding the Common Controls v6 manifest ensures the correct side-by-side DLL is loaded.
    println!("cargo:rustc-link-arg=/MANIFEST:EMBED");
    println!(
        "cargo:rustc-link-arg=/MANIFESTINPUT:{}",
        manifest_path.display()
    );
}

fn configure_windows_ndi_delay_load() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    if std::env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc") {
        return;
    }

    // Allow browser-port.exe to start even when the NDI runtime DLL is absent.
    println!("cargo:rustc-link-lib=delayimp");
    println!("cargo:rustc-link-arg=/DELAYLOAD:Processing.NDI.Lib.x64.dll");
}

fn build_windows_spout() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let mut build = cc::Build::new();
    build.cpp(true);
    build.include("native/spout");
    build.include("native/spout/SPOUTSDK/SPOUTSDK");
    build.include("native/spout/SPOUTSDK/SPOUTSDK/SpoutDirectX/SpoutDX/Tutorial04_Lib/include");
    build.file("native/spout/spout_bridge.cpp");
    println!("cargo:rerun-if-changed=native/spout/spout_bridge.cpp");

    for file in [
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/Spout.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutCopy.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutDirectX.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutFrameCount.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutGL.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutGLextensions.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutReceiver.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutSender.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutSenderNames.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutSharedMemory.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutGL/SpoutUtils.cpp",
        "native/spout/SPOUTSDK/SPOUTSDK/SpoutDirectX/SpoutDX/SpoutDX.cpp",
    ] {
        build.file(file);
        println!("cargo:rerun-if-changed={file}");
    }

    if cfg!(target_env = "msvc") {
        build.flag("/std:c++17");
    } else {
        build.flag("-std=c++17");
    }
    build.compile("browser_port_spout_bridge");

    for lib in [
        "User32", "Gdi32", "Dwmapi", "Dxgi", "D3D11", "D3D9", "Strmiids", "Shlwapi",
    ] {
        println!("cargo:rustc-link-lib={lib}");
    }
}

fn build_macos_syphon() {
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("macos") {
        return;
    }

    let mut build = cc::Build::new();
    build.cpp(true);
    build.file("native/syphon/syphon_bridge.mm");
    println!("cargo:rerun-if-changed=native/syphon/syphon_bridge.mm");
    if cfg!(target_env = "msvc") {
        build.flag("/std:c++17");
    } else {
        build.flag("-std=c++17");
    }
    build.flag("-fobjc-arc");
    build.compile("browser_port_syphon_bridge");

    for framework in [
        "Foundation",
        "AppKit",
        "ServiceManagement",
        "QuartzCore",
        "CoreMedia",
        "Metal",
        "OpenGL",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
