fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    build_windows_spout();
    build_macos_syphon();
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

    for lib in ["User32", "Gdi32", "Dwmapi", "Dxgi", "D3D11", "D3D9", "Strmiids", "Shlwapi"] {
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
    build.flag("-fobjc-arc");
    build.compile("browser_port_syphon_bridge");

    for framework in [
        "Foundation",
        "AppKit",
        "QuartzCore",
        "Metal",
        "OpenGL",
        "Syphon",
    ] {
        println!("cargo:rustc-link-lib=framework={framework}");
    }
}
