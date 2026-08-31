//! Native crash diagnostics: capture faulting IP on access violations.
//!
//! Registers a Vectored Exception Handler (first-chance, pass-through) that
//! logs the exception address, the owning module, and the offset within that
//! module for access violations. This makes crashes inside third-party PYDs
//! attributable without an external debugger.

#![allow(non_camel_case_types)]

use core::ffi::c_void;

const EXCEPTION_ACCESS_VIOLATION: u32 = 0xC000_0005;
const EXCEPTION_MAXIMUM_PARAMETERS: usize = 15;

#[repr(C)]
#[derive(Clone, Copy)]
struct EXCEPTION_RECORD64 {
    exception_code: u32,
    exception_flags: u32,
    exception_record: u64,
    exception_address: u64,
    number_parameters: u32,
    __pad: u32,
    exception_information: [u64; EXCEPTION_MAXIMUM_PARAMETERS],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct EXCEPTION_POINTERS {
    exception_record: *mut EXCEPTION_RECORD64,
    context_record: *mut CONTEXT64,
}

/// x64 CONTEXT — only the GPRs we need, at their exact Win32 offsets.
/// Layout per winnt.h: offset 0x78 Rax, 0x80 Rcx, 0x88 Rdx, 0x90 Rbx,
/// 0x98 Rsp, 0xA0 Rbp, 0xA8 Rsi, 0xB0 Rdi, then r8..r15.
#[repr(C)]
struct CONTEXT64 {
    __pad_head: [u64; 15], // 0x00..0x77 (PHome regs, float area start etc.)
    rax: u64,              // 0x78
    rcx: u64,              // 0x80
    rdx: u64,              // 0x88
    rbx: u64,              // 0x90
    rsp: u64,              // 0x98
    rbp: u64,              // 0xA0
    rsi: u64,              // 0xA8
    rdi: u64,              // 0xB0
    r8: u64,               // 0xB8
    r9: u64,               // 0xC0
    r10: u64,              // 0xC8
    r11: u64,              // 0xD0
    r12: u64,              // 0xD8
    r13: u64,              // 0xE0
    r14: u64,              // 0xE8
    r15: u64,              // 0xF0
    rip: u64,              // 0xF8
}

unsafe extern "system" {
    fn AddVectoredExceptionHandler(first: u32, handler: unsafe extern "system" fn(*mut EXCEPTION_POINTERS) -> i32) -> *mut c_void;
    fn GetModuleHandleExW(flags: u32, module_name: *const u16, module: *mut *mut c_void) -> i32;
    fn GetModuleFileNameW(module: *mut c_void, filename: *mut u16, size: u32) -> u32;
    fn GetCurrentProcess() -> *mut c_void;
}

#[repr(C)]
struct SYMBOL_INFO {
    size_of_struct: u32,
    type_index: u32,
    reserved: [u64; 2],
    index: u32,
    size: u32,
    mod_base: u64,
    flags: u32,
    __pad_flags: u32,
    value: u64,
    address: u64,
    register_: u32,
    scope: u32,
    tag: u32,
    name_len: u32,
    max_name_len: u32,
}

// offsetof(SYMBOL_INFO, Name) per the MS C header (ANSI build): all fixed
// fields total 84 bytes; the NUL-terminated char name follows there.
const SYMBOL_INFO_NAME_OFFSET: usize = 84;
const SYMBOL_INFO_SIZEOF_C: usize = 88; // includes Name[1] + tail padding

unsafe extern "system" {
    fn SymInitialize(process: *mut c_void, user_search_path: *const u16, invade_process: i32) -> i32;
    fn SymRefreshModuleList(process: *mut c_void) -> i32;
    fn SymFromAddr(
        process: *mut c_void,
        address: u64,
        displacement: *mut u64,
        symbol: *mut SYMBOL_INFO,
    ) -> i32;
}

static SYM_READY: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// When non-zero, `FfiResult::into_output` logs every NULL pointer result
/// that lacks a pending exception (a CPython contract violation class).
/// Armed by `_PyObject_New` when a C extension allocates a large object,
/// disarmed by the next successful Python-level call boundary.
pub static NULL_WINDOW: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// Best-effort symbol lookup for an instruction address. Returns an empty
/// string when dbghelp is unavailable or no symbol matches.
fn symbol_for_ip(ip: usize) -> String {
    const SYMBOL_BUF_NAME_MAX: usize = 256;
    // Fixed struct + inline ANSI name storage
    let mut buffer = vec![0u8; SYMBOL_INFO_SIZEOF_C + SYMBOL_BUF_NAME_MAX + 1];
    unsafe {
        let sym = buffer.as_mut_ptr() as *mut SYMBOL_INFO;
        core::ptr::write_bytes(buffer.as_mut_ptr(), 0, SYMBOL_INFO_SIZEOF_C);
        (*sym).size_of_struct = SYMBOL_INFO_SIZEOF_C as u32;
        (*sym).max_name_len = SYMBOL_BUF_NAME_MAX as u32;
        let process = GetCurrentProcess();
        if SYM_READY.load(core::sync::atomic::Ordering::Acquire) != 2 {
            // install() already tried eager init; if it failed, retry once here.
            if SYM_READY.load(core::sync::atomic::Ordering::Acquire) == 0 {
                if SymInitialize(process, core::ptr::null(), 1) != 0 {
                    SYM_READY.store(2, core::sync::atomic::Ordering::Release);
                } else {
                    SYM_READY.store(1, core::sync::atomic::Ordering::Release);
                    return String::new();
                }
            } else {
                return String::new();
            }
        }
        let mut displacement: u64 = 0;
        if SymFromAddr(process, ip as u64, &mut displacement, sym) != 0 {
            // Name is ANSI (char) NUL-terminated at offsetof(SYMBOL_INFO, Name).
            let name_ptr =
                buffer.as_ptr().add(SYMBOL_INFO_NAME_OFFSET) as *const u8;
            let max = SYMBOL_BUF_NAME_MAX;
            let mut name_len = 0usize;
            while name_len < max && *name_ptr.add(name_len) != 0 {
                name_len += 1;
            }
            let slice = core::slice::from_raw_parts(name_ptr, name_len);
            let name = String::from_utf8_lossy(slice).into_owned();
            // Exports-only resolution shows up as a huge displacement past a
            // small exported function; prefer the linker MAP file in that case.
            if displacement > 0x1000 {
                if let Some(map_sym) = map_symbol_for_ip(ip) {
                    return map_sym;
                }
            }
            return format!("{}+{:#x}", name, displacement as usize);
        }
    }
    // dbghelp unavailable entirely: try the MAP file.
    if let Some(map_sym) = map_symbol_for_ip(ip) {
        return map_sym;
    }
    String::new()
}

/// One symbol entry from the linker MAP file: RVA + demangled-ish name.
struct MapSym {
    rva: usize,
    name: String,
}

/// Lazily parsed linker MAP file (rustpython.map next to the exe), sorted by RVA.
fn map_symbols() -> Option<&'static [MapSym]> {
    use core::sync::atomic::{AtomicPtr, Ordering};
    static MAP: AtomicPtr<()> = AtomicPtr::new(core::ptr::null_mut());
    // Non-null pointer means parse already attempted; zero it to a sentinel box.
    struct MapHolder(Vec<MapSym>);
    let cur = MAP.load(Ordering::Acquire);
    if !cur.is_null() {
        let holder = unsafe { &*(cur as *const MapHolder) };
        return Some(&holder.0);
    }
    // Parse now (single-threaded race is acceptable for diagnostics: worst
    // case two threads each parse and one box wins the swap).
    let exe_path = {
        let mut buf = [0u16; 1024];
        let n = unsafe { GetModuleFileNameW(core::ptr::null_mut(), buf.as_mut_ptr(), 1024) } as usize;
        String::from_utf16_lossy(&buf[..n.min(buf.len())])
    };
    let exe = std::path::Path::new(&exe_path);
    // Cargo places the MAP next to the intermediate link output in deps/.
    let candidates = [
        exe.with_extension("map"),
        exe.with_extension("exe.map"),
        exe.parent().unwrap_or(std::path::Path::new(".")).join("deps").join(
            exe.file_stem().map(|s| s.to_os_string()).unwrap_or_default(),
        ),
    ];
    let mut text = None;
    for cand in candidates.iter() {
        // deps/<stem> needs .map appended
        let mut p = cand.clone().into_os_string();
        p.push(".map");
        let p = std::path::PathBuf::from(p);
        if let Ok(t) = std::fs::read_to_string(&p) {
            text = Some(t);
            break;
        }
    }
    let text = text?;
    let mut syms: Vec<MapSym> = Vec::with_capacity(200_000);
    for line in text.lines() {
        // Format: " 0001:00000010       symbol  140001010   f i v ..." — take
        // section:rva, then skip the (possibly decorated) name, address hex.
        let t = line.trim_start();
        if t.is_empty() || !t.as_bytes().first().is_some_and(|c| c.is_ascii_digit()) {
            continue;
        }
        let mut parts = t.split_whitespace();
        let seg = parts.next()?; // "0001:00000010"
        let rva_str = seg.split(':').nth(1)?;
        let Ok(rva) = usize::from_str_radix(rva_str, 16) else { continue };
        let name = match parts.next() {
            Some(nm) => nm,
            None => continue,
        };
        syms.push(MapSym { rva, name: name.to_owned() });
    }
    if syms.is_empty() {
        return None;
    }
    syms.sort_by_key(|s| s.rva);
    let boxed = Box::into_raw(Box::new(MapHolder(syms)));
    MAP.store(boxed as *mut (), Ordering::Release);
    let holder = unsafe { &*(boxed as *const MapHolder) };
    Some(&holder.0)
}

/// Resolve `ip` against the linker MAP file (exact attribution even for
/// static functions). Returns "<name>+<delta>" relative to the module base.
fn map_symbol_for_ip(ip: usize) -> Option<String> {
    let base = module_base_for_ip(ip)?;
    let syms = map_symbols()?;
    let rva = ip.wrapping_sub(base);
    let idx = syms.partition_point(|s| s.rva <= rva);
    if idx == 0 {
        return None;
    }
    let sym = &syms[idx - 1];
    Some(format!("{}+{:#x}", sym.name, rva - sym.rva))
}

/// Preferred image base of the module containing `ip` (HMODULE == base).
fn module_base_for_ip(ip: usize) -> Option<usize> {
    let mut handle: *mut c_void = core::ptr::null_mut();
    let ok = unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            ip as *const u16,
            &mut handle,
        )
    };
    if ok != 0 && !handle.is_null() {
        Some(handle as usize)
    } else {
        None
    }
}

const GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS: u32 = 0x4;
const GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT: u32 = 0x2;

static INSTALLED: core::sync::atomic::AtomicU8 = core::sync::atomic::AtomicU8::new(0);

/// Log the faulting module+offset for access violations, then continue
/// normal exception dispatch (handler never alters program behaviour).
unsafe extern "system" fn veh_handler(info: *mut EXCEPTION_POINTERS) -> i32 {
    if info.is_null() {
        return 0;
    }
    let rec_ptr = unsafe { (*info).exception_record };
    if rec_ptr.is_null() {
        return 0;
    }
    let rec = unsafe { *rec_ptr };
    if rec.exception_code != EXCEPTION_ACCESS_VIOLATION {
        return 0;
    }
    let ip = rec.exception_address as usize;
    let accessed = rec.exception_information.get(1).copied().unwrap_or(0) as usize;
    let kind = match rec.exception_information.first().copied().unwrap_or(0) {
        0 => "read",
        1 => "write",
        8 => "DEP-exec",
        _ => "?",
    };

    let mut handle: *mut c_void = core::ptr::null_mut();
    let module_info = if unsafe {
        GetModuleHandleExW(
            GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
            ip as *const u16,
            &mut handle,
        ) != 0
            && !handle.is_null()
    } {
        let mut name_buf = [0u16; 1024];
        let n = unsafe { GetModuleFileNameW(handle, name_buf.as_mut_ptr(), 1024) } as usize;
        let name_end = n.min(name_buf.len());
        let full = String::from_utf16_lossy(&name_buf[..name_end]);
        let base = handle as usize;
        let short = full.rsplit(['\\', '/']).next().unwrap_or(&full).to_owned();
        format!("{}+{:#x} (base {:#x})", short, ip.wrapping_sub(base), base)
    } else {
        "unknown module".to_owned()
    };

    eprint_diag(&format!(
        "NATIVE-CRASH: access violation ({}) of {:#x} at IP {:#x} in {} {} | fn='{}' rcx={:#x} rdx={:#x} rax={:#x} r8={:#x}",
        kind,
        accessed,
        ip,
        module_info,
        symbol_for_ip(ip),
        rustpython_vm::object::foreign_dispatch::current_fn_name(),
        unsafe { (*(*info).context_record).rcx },
        unsafe { (*(*info).context_record).rdx },
        unsafe { (*(*info).context_record).rax },
        unsafe { (*(*info).context_record).r8 },
    ));

    // Native backtrace: scan the stack for plausible return addresses
    // (values pointing into any loaded module) and attribute each.
    let rsp = unsafe { (*(*info).context_record).rsp } as *const usize;
    let mut frames = String::new();
    let mut count = 0;
    const MAX_FRAMES: usize = 12;
    for i in 1..4096 {
        if count >= MAX_FRAMES {
            break;
        }
        let candidate = unsafe { rsp.add(i).read_unaligned() };
        if candidate == 0 {
            continue;
        }
        // Does this value point into some loaded module?
        let mut handle: *mut c_void = core::ptr::null_mut();
        let in_module = unsafe {
            GetModuleHandleExW(
                GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                candidate as *const u16,
                &mut handle,
            ) != 0 && !handle.is_null()
        };
        if !in_module {
            continue;
        }
        // Skip the immediate faulting frame region near RIP.
        if candidate.abs_diff(ip) < 0x100 {
            continue;
        }
        let base = handle as usize;
        let short = {
            let mut nb = [0u16; 1024];
            let n = unsafe { GetModuleFileNameW(handle, nb.as_mut_ptr(), 1024) } as usize;
            let full = String::from_utf16_lossy(&nb[..n.min(nb.len())]);
            full.rsplit(['\\', '/']).next().unwrap_or(&full).to_owned()
        };
        let sym = map_symbol_for_ip(candidate)
            .unwrap_or_else(|| format!("{:#x}", candidate.wrapping_sub(base)));
        frames.push_str(&format!("\n  #{} {} +{}", count, short, sym));
        count += 1;
    }
    if !frames.is_empty() {
        crate::crash_diag::eprint_diag(&format!("NATIVE-BACKTRACE:{}", frames));
    }
    0 // continue search — do not swallow the exception
}

/// Install the diagnostics handler once per process. Also eagerly initializes
/// dbghelp symbol resolution (SymInitialize) OUTSIDE any exception context —
/// calling it during exception dispatch (loader lock) silently skips PDB
/// loading, which is why symbols previously resolved to exports only.
pub fn install() {
    if INSTALLED.load(core::sync::atomic::Ordering::Acquire) == 0
        && INSTALLED
            .compare_exchange(0, 1, core::sync::atomic::Ordering::AcqRel, core::sync::atomic::Ordering::Acquire)
            .is_ok()
    {
        unsafe {
            // Eager SymInitialize with exe-dir search path so PDBs load now.
            let mut exe_buf = [0u16; 1024];
            let n = GetModuleFileNameW(core::ptr::null_mut(), exe_buf.as_mut_ptr(), 1024) as usize;
            let mut dir_len = n.min(exe_buf.len());
            while dir_len > 0
                && exe_buf[dir_len - 1] != u16::from(b'\\')
                && exe_buf[dir_len - 1] != u16::from(b'/')
            {
                dir_len -= 1;
            }
            exe_buf[dir_len] = 0;
            let process = GetCurrentProcess();
            if SymInitialize(process, exe_buf.as_ptr(), 1) != 0 {
                // Force-load symbols for every module (incl. the exe's PDB)
                // now, outside any exception context.
                SymRefreshModuleList(process);
                SYM_READY.store(2, core::sync::atomic::Ordering::Release);
            }
            AddVectoredExceptionHandler(0, veh_handler);
        }
    }
}

/// Lock-free stderr diagnostic print (safe to call from an exception context).
fn eprint_diag(msg: &str) {
    use core::sync::atomic::{AtomicBool, Ordering};
    static BUSY: AtomicBool = AtomicBool::new(false);
    if !BUSY.swap(true, Ordering::SeqCst) {
        // std eprintln is not signal-safe but is acceptable for diagnostics
        eprintln!("{}", msg);
        BUSY.store(false, Ordering::Release);
    }
}