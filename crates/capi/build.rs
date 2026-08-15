//! Compiles the small C shim that provides C-variadic entry points
//! (PyArg_ParseTuple, Py_BuildValue, PyObject_CallMethod, ...), which stable
//! Rust cannot define. See shim/getargs.c and crates/capi/src/arg.rs.

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=shim/getargs.c");

    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let target_arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    // The shim only needs a C compiler; skip targets that cannot build one.
    if target_arch == "wasm32" {
        return;
    }
    if target_env == "musl" {
        return;
    }

    let out_dir = PathBuf::from(std::env::var_os("OUT_DIR").expect("OUT_DIR is set by cargo"));
    let mut build = cc::Build::new();
    build
        .file("shim/getargs.c")
        .warnings(true)
        .compile("rp_getargs");

    println!("cargo:rustc-link-search=native={}", out_dir.display());
}
