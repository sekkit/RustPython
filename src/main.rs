use rustpython::{InterpreterBuilder, InterpreterBuilderExt};

// mimalloc global allocator: Python workloads are allocation-heavy
// (every object is a heap allocation); mimalloc's per-thread arenas and
// size-class bins outperform the Windows HeapAlloc default by 10-30%
// on allocation-dense benchmarks.
#[global_allocator]
static GLOBAL_ALLOCATOR: mimalloc::MiMalloc = mimalloc::MiMalloc;

pub fn main() -> std::process::ExitCode {
    let mut config = InterpreterBuilder::new();
    #[cfg(feature = "stdlib")]
    {
        config = config.init_stdlib();
    }
    rustpython::run(config)
}
