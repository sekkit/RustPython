fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    let target = std::env::var("CARGO_CFG_TARGET_OS").unwrap();
    let capi_enabled = std::env::var_os("CARGO_FEATURE_CAPI").is_some();

    match target.as_str() {
        "linux" if capi_enabled => {
            println!("cargo:rustc-link-arg-bin=rustpython=-Wl,--export-dynamic");
        }
        "macos" if capi_enabled => {
            println!("cargo:rustc-link-arg-bin=rustpython=-Wl,-export_dynamic");
        }
        "windows" => {
            println!("cargo:rerun-if-changed=logo.ico");
            let mut res = winresource::WindowsResource::new();
            if std::path::Path::new("logo.ico").exists() {
                res.set_icon("logo.ico");
            } else {
                println!("cargo:warning=logo.ico not found, skipping icon embedding");
                return;
            }
            res.compile()
                .map_err(|e| {
                    println!("cargo:warning=Failed to compile Windows resources: {e}");
                })
                .ok();

            // Export every C-API symbol from the executable itself. The capi
            // cdylib is built first (it is a default member), so its export
            // table is authoritative; exporting from the exe means extension
            // calls run in the exe's module and share its thread-local VM.
            if capi_enabled {
                export_windows_capi_symbols();
            }
        }
        _ => {}
    }
}

fn export_windows_capi_symbols() {
    let target_dir = std::env::var_os("CARGO_TARGET_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("target"));
    let capi_dll = target_dir.join("release").join("rustpython_capi.dll");
    let Some(exports) = read_pe_exports(&capi_dll) else {
        println!(
            "cargo:warning=rustpython_capi.dll not found ({}); the executable will not export C-API symbols",
            capi_dll.display()
        );
        return;
    };

    let out_dir = std::path::PathBuf::from(std::env::var_os("OUT_DIR").unwrap());
    let def_path = out_dir.join("rustpython_exports.def");
    let mut def = String::from("EXPORTS\n");
    for name in &exports {
        def.push_str(name);
        def.push('\n');
    }
    std::fs::write(&def_path, def).expect("failed to write exports def");
    println!("cargo:rerun-if-changed={}", capi_dll.display());
    println!(
        "cargo:rustc-link-arg-bin=rustpython=/DEF:{}",
        def_path.display()
    );
}

/// Read the export name table of a PE (DLL/EXE) file.
fn read_pe_exports(path: &std::path::Path) -> Option<Vec<String>> {
    let data = std::fs::read(path).ok()?;
    if data.len() < 0x40 || &data[0..2] != b"MZ" {
        return None;
    }
    let e_lfanew = u32::from_le_bytes(data[0x3C..0x40].try_into().ok()?) as usize;
    if data.len() < e_lfanew + 24 || &data[e_lfanew..e_lfanew + 4] != b"PE\0\0" {
        return None;
    }
    let pe = e_lfanew + 4;
    let num_sections = u16::from_le_bytes(data[pe + 2..pe + 4].try_into().ok()?) as usize;
    let opt_size = u16::from_le_bytes(data[pe + 16..pe + 18].try_into().ok()?) as usize;
    let opt = pe + 20;
    if data.len() < opt + opt_size {
        return None;
    }
    let magic = u16::from_le_bytes(data[opt..opt + 2].try_into().ok()?);
    // Export directory is data directory index 0.
    let export_rva = match magic {
        0x20B => u32::from_le_bytes(data[opt + 112..opt + 116].try_into().ok()?),
        0x10B => u32::from_le_bytes(data[opt + 96..opt + 100].try_into().ok()?),
        _ => return None,
    } as usize;
    if export_rva == 0 {
        return None;
    }
    let sections = opt + opt_size;
    let section = |rva: usize| -> (usize, usize) {
        for i in 0..num_sections {
            let s = sections + i * 40;
            if s + 40 > data.len() {
                continue;
            }
            let va = u32::from_le_bytes(
                data[s + 12..s + 16].try_into().expect("slice is 4 bytes"),
            ) as usize;
            let vs = u32::from_le_bytes(
                data[s + 8..s + 12].try_into().expect("slice is 4 bytes"),
            ) as usize;
            let raw = u32::from_le_bytes(
                data[s + 20..s + 24].try_into().expect("slice is 4 bytes"),
            ) as usize;
            if rva >= va && rva < va + vs {
                return (raw + (rva - va), vs - (rva - va));
            }
        }
        (0, 0)
    };
    let (exp_off, exp_size) = section(export_rva);
    if exp_off == 0 || exp_size < 40 {
        return None;
    }
    let num_names =
        u32::from_le_bytes(data[exp_off + 24..exp_off + 28].try_into().ok()?) as usize;
    let names_rva = u32::from_le_bytes(data[exp_off + 32..exp_off + 36].try_into().ok()?) as usize;
    let (names_off, names_size) = section(names_rva);
    if names_off == 0 {
        return None;
    }
    let mut out = Vec::with_capacity(num_names);
    for i in 0..num_names {
        let idx = names_off + i * 4;
        if idx + 4 > names_off + names_size {
            break;
        }
        let name_rva = u32::from_le_bytes(data[idx..idx + 4].try_into().ok()?) as usize;
        let (name_off, name_size) = section(name_rva);
        if name_off == 0 || name_size == 0 {
            continue;
        }
        let end = data[name_off..name_off + name_size]
            .iter()
            .position(|&b| b == 0)
            .unwrap_or(name_size);
        if let Ok(name) = std::str::from_utf8(&data[name_off..name_off + end]) {
            out.push(name.to_owned());
        }
    }
    Some(out)
}
