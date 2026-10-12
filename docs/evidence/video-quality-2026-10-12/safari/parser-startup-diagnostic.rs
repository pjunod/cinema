use std::{env, fs, time::Instant};
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<String> = env::args().collect();
    let bytes = fs::read(&args[1])?;
    let mut config = wasmtime::Config::default();
    config.epoch_interruption(true).wasm_memory64(false).wasm_multi_memory(false)
        .wasm_simd(true).wasm_relaxed_simd(false).wasm_tail_call(false)
        .wasm_custom_page_sizes(false).max_wasm_stack(1024 * 1024);
    if args.get(2).map(String::as_str) == Some("no-opt") {
        config.cranelift_opt_level(wasmtime::OptLevel::None);
    }
    let start = Instant::now();
    let engine = wasmtime::Engine::new(&config)?;
    eprintln!("engine_ms={:.3}", start.elapsed().as_secs_f64()*1000.0);
    let start = Instant::now();
    let module = wasmtime::Module::new(&engine, &bytes)?;
    println!("module_ms={:.3} bytes={} imports={}", start.elapsed().as_secs_f64()*1000.0,bytes.len(),module.imports().count());
    Ok(())
}
