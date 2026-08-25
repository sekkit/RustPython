## Instance allocation freelist (analysis complete, design pending)

Feasible: PyInner allocation size is FIXED for all instances (ObjExt is
constant-size; slots array lives in a Box on the heap), so a pool does
not need size classes.

Hard constraints discovered (round 101):
1. Existing freelist push filter requires `typ == T::class()`; user-class
   instances are heap types -> never eligible today. Must relax with
   justification for heaptype instances (safe: __del__ ran inside clear,
   published-object check excludes type-attribute leaks).
2. ObjExt.slots Box length == member_count of the class AT PUSH TIME.
   Pool reuse across classes with different member_count would mismatch
   the Box length -> must pool per member_count (or only pool
   member_count == 0 instances) AND validate on pop.
3. pop must rewrite ObjExt.dict (InstanceDict) — the generic pop path
   only rewrites ref_count/gc_bits/payload/typ. Needs a PyPayload
   restore hook.
4. GC: instances are tracked; dealloc untracks before push (existing
   freelist path already requires immediate untracking, verified).

Do this as its own focused engineering pass; class_method pure-create
gap is 6.9x of which allocation is ~60-70%.

# Performance Optimization Roadmap: Lessons from Mojo & CPython

## Executive Summary

RustPython's benchmark shows uniform 7-11x dispatch overhead on micro-ops
(int_add 8.3x, fn_call 9.6x) while bulk operations that stay in a single
Rust call are at parity or faster (str_count 0.5x FASTER than CPython,
str_replace 0.9x). The bottleneck is the bytecode dispatch loop, not the
object model. This document analyzes how Mojo/Modular and CPython solved
their performance problems and maps those lessons to concrete RustPython
optimizations.

## Case Study 1: CPython 3.13 Adaptive Specializing Interpreter

CPython gained 25-60% speedup (Faster CPython project) via three tiers:

### Tier 0: Original interpreter
Generic dispatch: every BINARY_OP goes through opcode table lookup,
stack manipulation, generic binary function.

### Tier 1: Specializing adaptive interpreter (PEP 659)
- Quickening: after ~8 executions, hot bytecodes are REPLACED in-place
  with specialized versions (BINARY_OP -> BINARY_OP_ADD_INT)
- Inline caches: each specialized instruction embeds type/version data
  directly in the bytecode stream (no dict lookups per op)
- Deopt guards: one pointer compare; on mismatch, fall back to Tier 0
- Key insight: 95%+ of dynamic ops become monomorphic after warmup

### Tier 2: Copy-and-patch JIT
- Pre-compiled stencils per instruction, stitched together at runtime
- No LLVM at runtime; just memcpy of machine code fragments + patching
- ~5% further gain over Tier 1; low complexity vs full JIT

**Lesson for RustPython**: Implement quickening + inline caches BEFORE
any JIT. It is pure Rust, no unsafe machine code generation needed, and
delivers the largest single win.

## Case Study 2: Mojo / Modular MAX

Mojo achieves Python-level productivity with C-level performance through:

### Value semantics + ownership (no refcounting in hot loops)
- `owned` / `borrowed` / `inout` argument conventions
- No atomic reference counting inside compute kernels
- Contrast: every Python object operation pays refcount traffic

### MLIR-based compilation stack
- Kernel fusion across operations (graph compiler)
- Autotuning: parameterized kernels search tile sizes/vector widths
  at compile time or first run (`autotune` keyword)
- SIMD native: `simd_load`/`simd_store` types map to hardware vectors

### Python interop strategy
- MAX Engine imports Python models and traces them to a graph
- Zero-copy interop: Mojo objects can wrap Python buffers without copies
- Key insight: don't make Python faster; identify hot regions and
  replace them with compiled equivalents

**Lesson for RustPython**:
1. Eliminate refcount traffic on hot paths (arena allocation for
   short-lived temporaries during expression evaluation)
2. Add SIMD fast paths for bulk string/list operations
3. Consider tracing JIT for hot loops (identify loops via counter)

## Case Study 3: Wasmtime/Cranelift (Rust-native JIT)

Wasmtime proves that a Rust-hosted JIT compiler is viable:
- Cranelift: SSA-based IR, register allocation, fast compilation
- Pulley: portable interpreter bytecode as a fallback target
- Balancing: compile-time vs execution-time tradeoff tunable per tier

**Lesson**: If adding a JIT, consider Cranelift rather than writing
machine code emitters by hand. Integration point: compile CodeObject
to Cranelift IR, patch calls to runtime helpers.

## Case Study 4: Rust interpreter dispatch techniques

From Rust internals discussions and projects:

### Current RustPython dispatch (match statement)
```rust
loop {
    let op = frame.read_instruction();
    match op {
        Opcode::BinaryOp(op) => { ... }
        // 100+ arms
    }
}
```
The match compiles to a jump table, but each arm has bounds checks,
enum discriminant loads, and cache-unfriendly code layout.

### Option A: Tail-call dispatch (become keyword, nightly)
```rust
fn dispatch(frame) -> ! {
    match op {
        BinaryOp => { ...; return dispatch(next); }  // TCO
    }
}
```
With `-Ztai-calls=direct`, each arm becomes a separate function;
the CPU's branch predictor learns per-opcode patterns instead of
one shared indirect branch.
Measured gains in other Rust interpreters: 10-30%.

### Option B: Computed goto equivalent
Rust lacks computed goto; the closest is a loop of labeled blocks
(unstable `label_break_value`) or a macro-generated state machine.
Lower priority than Option A.

### Option C: Superinstructions
Fuse common sequences at compile time:
- LOAD_FAST + LOAD_FAST + BINARY_OP_ADD -> BINARY_ADD_LOCALS
- LOAD_FAST + STORE_FAST -> MOVE_FAST
- FOR_ITER + STORE_FAST -> FOR_ITER_STORE
Reduces dispatch count by 20-40% for typical code. Pure win, no
unsafe required, works today on stable Rust.

## Prioritized Roadmap for RustPython

### Phase 1: Superinstructions (est. 1-2 sessions, stable Rust)
- Fuse LOAD_FAST pairs before binary ops in CodeObject construction
- Fuse FOR_ITER + STORE_FAST
- Expected gain: 15-30% on iteration-heavy benchmarks

### Phase 2: Inline caches for attribute access (est. 2-4 sessions)
- Add version-tagged inline caches to LOAD_ATTR/STORE_ATTR
- Cache (type_version, method_offset) pairs in bytecode operands
- Expected gain: 30-50% on method-heavy code

### Phase 3: Quickening/specialization (est. 3-6 sessions)
- Execution counters per instruction (saturating u8)
- On threshold, replace with specialized variant:
  - BINARY_OP -> ADD_INT/ADD_FLOAT/ADD_STR (guarded by type ptr eq)
  - COMPARE_OP -> EQ_INT etc.
- Requires mutable bytecode buffer (already have CodeObject)
- Expected gain: 20-50% additional on monomorphic hot code

### Phase 4: Tail-call dispatch (est. 1 session + nightly pin)
- Refactor eval loop into per-instruction functions with `become`
- Measure; keep only if >10% gain
- Expected gain: 10-30%

### Phase 5: Arena allocation for temporaries (est. 2-3 sessions)
- Expression evaluation creates many short-lived PyRefs
- Arena/bump allocator cleared at statement boundary eliminates
  refcount atomics for these
- Expected gain: 10-20% overall, more on allocation-heavy code

### Phase 6: SIMD string paths (est. 1-2 sessions)
- memchr already used in places; extend to count/index/find
- Use core::core_arch x86_64 intrinsics behind cfg for split/count
- Expected gain: 2-5x on bulk string benchmarks (already partially there)

## What NOT to do (lessons from failures)

- Do not attempt LLVM-at-runtime (compile time kills warmup wins)
- Do not specialize without deopt guards (correctness bugs)
- Do not skip Phase 1/2 to chase the JIT (CPython data shows Tier 1
  delivers most of the total win)

## Delivered (2026-08 session)

- **Neumaier-compensated `sum()`** — verbatim port of CPython main's
  `CompensatedSum` machinery for float and complex chains, including the
  int→float→complex fall-through and C11-Annex-G mixed-mode rules that
  preserve imaginary signed zeros. List/tuple inputs scan storage
  zero-copy; the compensated prefix is materialized on bail-out exactly
  like CPython's exit from its specialized loops. Port note: CPython's
  `total.lo += (hi - t) + x` binds looser than `+`; translating it as
  `(lo + (hi - t)) + x` lets huge corrections swallow the compensation
  via rounding — keep the parentheses.
- **Header-only PyStr (SSO)** — short strings (any WTF-8) ≤ 22 bytes live inside
  the PyStr header (`StrStorage::Sso`, sized to the Vec slot it replaces);
  creation/clone/drop of short strings involve no heap allocation.
- **Biased reference counting** — ATTEMPTED AND WITHDRAWN. Plain
  (non-atomic) count updates gated on a single-thread-mode flag crashed
  `--features threading` builds inside `default_dealloc<PyType>`
  (`hint::unreachable_unchecked` on a NULL type deref during teardown):
  replacing locked RMWs with plain stores lets the compiler move the
  count store across teardown calls, re-opening a latent ordering
  hazard in object destruction. Reverted in
  88db6cbea; a future attempt must first make dealloc ordering
  independent of when the count store lands (or use deferred RC).
- **Superinstruction status** — LOAD_FAST_LOAD_FAST,
  STORE_FAST_LOAD_FAST, STORE_FAST_STORE_FAST and
  LOAD_FAST_BORROW_LOAD_FAST_BORROW are emitted by codegen and executed
  (borrow loads currently fall back to clones at runtime).

## References
- Faster CPython: https://github.com/faster-cpython
- PEP 659 (specializing adaptive): https://peps.python.org/pep-0659/
- CPython JIT (copy-patch): https://lwn.net/Articles/958350/
- Mojo: https://github.com/modular/modular
- Cranelift: https://wasmtime.dev/
- Rust TCO experiments: https://internals.rust-lang.org/t/4668