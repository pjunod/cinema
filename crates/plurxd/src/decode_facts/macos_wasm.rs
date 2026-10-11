//! Capability-only WASI adapter for the packaged source parser.
//!
//! WASI descriptor numbers are virtual: none are host descriptors. The sole
//! virtual directory contains exactly `/dev/fd/3`, backed by the preparation
//! owner's held file. No generic WASI implementation, directory, environment,
//! process, network or writable file capability is installed.
#[path = "macos_wasm/signing.rs"]
mod signing;

use std::collections::BTreeMap;
use std::fs::File;
use std::os::unix::fs::FileExt;
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};
use wasmtime::{
    Caller, Config, Engine, Error, ExternType, Linker, Memory, Module, Store, StoreLimits,
    StoreLimitsBuilder, Val,
};

pub const ABI_ID: &str = "plurx-source-wasi-p1-v1-wasmtime-49.0.2";
const MEMORY_LIMIT: usize = 512 * 1024 * 1024;
const IO_LIMIT: usize = 64 * 1024;
const BADF: i32 = 8;
const INVAL: i32 = 28;
const NOENT: i32 = 44;
const NOTCAPABLE: i32 = 76;

#[derive(Debug)]
pub struct Parser {
    engine: Engine,
    module: Module,
}

pub struct Output {
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub status: i32,
}

struct State {
    source: Option<Arc<File>>,
    source_size: u64,
    cursors: BTreeMap<i32, u64>,
    args: Vec<Vec<u8>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    stdout_limit: usize,
    stderr_limit: usize,
    deadline: Instant,
    stop: Arc<std::sync::atomic::AtomicBool>,
    limits: StoreLimits,
    memory: Option<Memory>,
}

impl Parser {
    pub fn new(bytes: &[u8]) -> Result<Self, String> {
        if bytes.len() > 32 * 1024 * 1024 || bytes.get(..8) != Some(b"\0asm\x01\0\0\0") {
            return Err("source parser must be a bounded WebAssembly v1 image".into());
        }
        refuse_start_section(bytes)?;
        signing::require_execution_policy()?;
        let mut config = Config::default();
        config
            .epoch_interruption(true)
            .wasm_memory64(false)
            .wasm_multi_memory(false)
            .wasm_simd(true)
            .wasm_relaxed_simd(false)
            .wasm_tail_call(false)
            .wasm_custom_page_sizes(false)
            .max_wasm_stack(1024 * 1024);
        let engine = Engine::new(&config).map_err(|e| e.to_string())?;
        let module = Module::new(&engine, bytes).map_err(|e| e.to_string())?;
        // Every import must belong to this small adapter. In particular there
        // are no imported memories/tables, native callbacks or WASI extensions.
        for import in module.imports() {
            if import.module() != "wasi_snapshot_preview1"
                || !allowed(import.name())
                || !matches!(import.ty(), ExternType::Func(_))
            {
                return Err(format!(
                    "unavailable parser capability: {}::{}",
                    import.module(),
                    import.name()
                ));
            }
        }
        Ok(Self { engine, module })
    }

    pub fn run(
        &self,
        source: Option<Arc<File>>,
        args: Vec<String>,
        output_limits: (usize, usize),
        deadline: Instant,
        stop: Arc<std::sync::atomic::AtomicBool>,
    ) -> Result<Output, String> {
        let source_size = source
            .as_ref()
            .map(|f| f.metadata().map(|m| m.len()))
            .transpose()
            .map_err(|e| e.to_string())?
            .unwrap_or(0);
        let state = State {
            source,
            source_size,
            cursors: BTreeMap::new(),
            args: std::iter::once("ffprobe".to_owned())
                .chain(args)
                .map(|s| s.into_bytes())
                .collect(),
            stdout: Vec::new(),
            stderr: Vec::new(),
            stdout_limit: output_limits.0,
            stderr_limit: output_limits.1,
            deadline,
            stop,
            memory: None,
            limits: StoreLimitsBuilder::new()
                .memory_size(MEMORY_LIMIT)
                .instances(1)
                .memories(1)
                .tables(1)
                .table_elements(65536)
                .trap_on_grow_failure(false)
                .build(),
        };
        let mut store = Store::new(&self.engine, state);
        store.limiter(|s| &mut s.limits);
        let mut linker = Linker::new(&self.engine);
        for import in self.module.imports() {
            let ExternType::Func(ty) = import.ty() else {
                return Err("non-function parser import".into());
            };
            let name = import.name().to_owned();
            let key = name.clone();
            linker
                .func_new(
                    import.module(),
                    &key,
                    ty.clone(),
                    move |mut caller, input, result| {
                        check(caller.data())?;
                        let errno = dispatch(&name, &mut caller, input)?;
                        check(caller.data())?;
                        if let Some(result) = result.first_mut() {
                            *result = Val::I32(errno);
                        }
                        Ok(())
                    },
                )
                .map_err(|e| e.to_string())?;
        }
        store.set_epoch_deadline(1);
        store.epoch_deadline_callback(|context| {
            check(context.data())?;
            Ok(wasmtime::UpdateDeadline::Continue(1))
        });
        // The runtime's epoch clock owns no media capability. It is scoped to
        // this execution and joined before source/admission ownership returns.
        let clock = EpochClock::new(self.engine.clone()).map_err(|e| e.to_string())?;
        let instance = linker
            .instantiate(&mut store, &self.module)
            .map_err(error_reason)?;
        store.data_mut().memory = instance.get_memory(&mut store, "memory");
        let start = instance
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(error_reason)?;
        let status = match start.call(&mut store, ()) {
            Ok(()) => 0,
            Err(error) => match error.downcast_ref::<ExitCode>() {
                Some(exit) => exit.0,
                None => return Err(error_reason(error)),
            },
        };
        check(store.data()).map_err(error_reason)?;
        drop(clock);
        let state = store.into_data();
        Ok(Output {
            stdout: state.stdout,
            stderr: state.stderr,
            status,
        })
    }
}

#[derive(Debug)]
struct ExitCode(i32);
impl std::fmt::Display for ExitCode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "source parser exit {}", self.0)
    }
}
impl std::error::Error for ExitCode {}
fn error_reason(error: Error) -> String {
    error.root_cause().to_string()
}

struct EpochClock {
    stop: Arc<std::sync::atomic::AtomicBool>,
    thread: Option<std::thread::JoinHandle<()>>,
}
impl EpochClock {
    fn new(engine: Engine) -> std::io::Result<Self> {
        let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let owned = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("source-parser-epoch".into())
            .spawn(move || {
                while !owned.load(std::sync::atomic::Ordering::Relaxed) {
                    engine.increment_epoch();
                    std::thread::park_timeout(std::time::Duration::from_millis(5));
                }
            })?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}
impl Drop for EpochClock {
    fn drop(&mut self) {
        self.stop.store(true, std::sync::atomic::Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            thread.thread().unpark();
            let _ = thread.join();
        }
    }
}

/// Instantiation may initialize bounded memory/data, but must not execute guest
/// code outside the execution deadline owner.
fn refuse_start_section(bytes: &[u8]) -> Result<(), String> {
    let mut cursor = 8usize;
    while cursor < bytes.len() {
        let section = bytes[cursor];
        cursor += 1;
        let mut size = 0u32;
        let mut done = false;
        for shift in [0, 7, 14, 21, 28] {
            let value = *bytes.get(cursor).ok_or("truncated parser section")?;
            cursor += 1;
            if shift == 28 && value & 0xf0 != 0 {
                return Err("parser section size overflow".into());
            }
            size |= u32::from(value & 0x7f) << shift;
            if value & 0x80 == 0 {
                done = true;
                break;
            }
        }
        if !done || section == 8 {
            return Err("parser must not execute a module start section".into());
        }
        let cap = match section {
            1 => Some(4096),
            2 => Some(32),
            3 => Some(24_000),
            4 | 5 => Some(1),
            6 => Some(256),
            7 => Some(8),
            9 | 11 => Some(64),
            _ => None,
        };
        if let Some(cap) = cap {
            let mut count = 0u32;
            let mut count_done = false;
            for (at, shift) in (cursor..).zip([0, 7, 14, 21, 28]) {
                let value = *bytes.get(at).ok_or("truncated parser section count")?;
                if shift == 28 && value & 0xf0 != 0 {
                    return Err("parser section count overflow".into());
                }
                count |= u32::from(value & 0x7f) << shift;
                if value & 0x80 == 0 {
                    count_done = true;
                    break;
                }
            }
            if !count_done || count > cap {
                return Err("parser module structure exceeds bound".into());
            }
        }
        cursor = cursor
            .checked_add(size as usize)
            .filter(|v| *v <= bytes.len())
            .ok_or("truncated parser section")?;
    }
    Ok(())
}

fn check(state: &State) -> Result<(), Error> {
    if state.stop.load(std::sync::atomic::Ordering::Relaxed) {
        return Err(Error::msg("parser cancelled"));
    }
    if Instant::now() >= state.deadline {
        return Err(Error::msg("parser deadline"));
    }
    Ok(())
}

fn allowed(name: &str) -> bool {
    matches!(
        name,
        "args_get"
            | "args_sizes_get"
            | "environ_get"
            | "environ_sizes_get"
            | "clock_time_get"
            | "fd_close"
            | "fd_fdstat_get"
            | "fd_fdstat_set_flags"
            | "fd_filestat_get"
            | "fd_pread"
            | "fd_prestat_get"
            | "fd_prestat_dir_name"
            | "fd_read"
            | "fd_readdir"
            | "fd_seek"
            | "fd_write"
            | "path_filestat_get"
            | "path_open"
            | "path_remove_directory"
            | "path_rename"
            | "path_unlink_file"
            | "poll_oneoff"
            | "proc_exit"
    )
}

fn memory(c: &Caller<'_, State>) -> Result<Memory, Error> {
    c.data()
        .memory
        .ok_or_else(|| Error::msg("parser lacks memory"))
}
fn read(c: &Caller<'_, State>, ptr: i32, size: usize) -> Result<Vec<u8>, Error> {
    if size > IO_LIMIT {
        return Err(Error::msg("parser host read exceeds bound"));
    }
    let mut bytes = vec![0; size];
    memory(c)?
        .read(c, ptr as u32 as usize, &mut bytes)
        .map_err(|e| Error::msg(e.to_string()))?;
    Ok(bytes)
}
fn write(c: &mut Caller<'_, State>, ptr: i32, bytes: &[u8]) -> Result<(), Error> {
    memory(c)?
        .write(c, ptr as u32 as usize, bytes)
        .map_err(|e| Error::msg(e.to_string()))
}
fn u32_at(c: &Caller<'_, State>, ptr: i32) -> Result<u32, Error> {
    Ok(u32::from_le_bytes(
        read(c, ptr, 4)?
            .try_into()
            .map_err(|_| Error::msg("invalid u32"))?,
    ))
}
fn ptr(base: i32, offset: usize) -> Result<i32, Error> {
    (base as u32 as usize)
        .checked_add(offset)
        .and_then(|v| u32::try_from(v).ok())
        .map(|v| v as i32)
        .ok_or_else(|| Error::msg("parser pointer overflow"))
}
fn i(args: &[Val], index: usize) -> Result<i32, Error> {
    args.get(index)
        .and_then(Val::i32)
        .ok_or_else(|| Error::msg("parser import signature mismatch"))
}
fn l(args: &[Val], index: usize) -> Result<i64, Error> {
    args.get(index)
        .and_then(Val::i64)
        .ok_or_else(|| Error::msg("parser import signature mismatch"))
}
fn source_path(c: &Caller<'_, State>, fd: i32, p: i32, len: i32) -> Result<bool, Error> {
    Ok(fd == 3 && c.data().source.is_some() && len == 8 && read(c, p, 8)? == b"dev/fd/3")
}
fn stat(c: &mut Caller<'_, State>, out: i32, directory: bool) -> Result<(), Error> {
    let mut bytes = [0; 64];
    bytes[16] = if directory { 3 } else { 4 };
    bytes[24..32].copy_from_slice(&1u64.to_le_bytes());
    bytes[32..40].copy_from_slice(&c.data().source_size.to_le_bytes());
    write(c, out, &bytes)
}

fn dispatch(name: &str, c: &mut Caller<'_, State>, a: &[Val]) -> Result<i32, Error> {
    match name {
        "proc_exit" => return Err(Error::new(ExitCode(i(a, 0)?))),
        "args_sizes_get" => {
            let count = c.data().args.len() as u32;
            let size = c.data().args.iter().map(|s| s.len() + 1).sum::<usize>() as u32;
            write(c, i(a, 0)?, &count.to_le_bytes())?;
            write(c, i(a, 1)?, &size.to_le_bytes())?;
        }
        "args_get" => {
            let args = c.data().args.clone();
            let mut offset = 0;
            for (n, arg) in args.iter().enumerate() {
                let target = ptr(i(a, 1)?, offset)?;
                write(c, ptr(i(a, 0)?, n * 4)?, &(target as u32).to_le_bytes())?;
                write(c, target, arg)?;
                write(c, ptr(target, arg.len())?, &[0])?;
                offset += arg.len() + 1;
            }
        }
        "environ_sizes_get" => {
            write(c, i(a, 0)?, &[0; 4])?;
            write(c, i(a, 1)?, &[0; 4])?;
        }
        "environ_get" => {}
        "clock_time_get" => {
            let clock = i(a, 0)?;
            if !(0..=3).contains(&clock) {
                return Ok(INVAL);
            }
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_err(|e| Error::msg(e.to_string()))?
                .as_nanos() as u64;
            write(c, i(a, 2)?, &nanos.to_le_bytes())?;
        }
        "fd_prestat_get" => {
            if i(a, 0)? != 3 || c.data().source.is_none() {
                return Ok(BADF);
            }
            write(c, i(a, 1)?, &[0, 0, 0, 0, 1, 0, 0, 0])?;
        }
        "fd_prestat_dir_name" => {
            if i(a, 0)? != 3 || i(a, 2)? < 1 || c.data().source.is_none() {
                return Ok(BADF);
            }
            write(c, i(a, 1)?, b"/")?;
        }
        "path_open" => {
            if !source_path(c, i(a, 0)?, i(a, 2)?, i(a, 3)?)? {
                return Ok(NOENT);
            }
            if i(a, 4)? != 0 || l(a, 5)? & (1 << 6) != 0 || i(a, 7)? != 0 {
                return Ok(NOTCAPABLE);
            }
            let Some(fd) = (4..12).find(|fd| !c.data().cursors.contains_key(fd)) else {
                return Ok(BADF);
            };
            c.data_mut().cursors.insert(fd, 0);
            write(c, i(a, 8)?, &(fd as u32).to_le_bytes())?;
        }
        "path_filestat_get" => {
            if !source_path(c, i(a, 0)?, i(a, 2)?, i(a, 3)?)? {
                return Ok(NOENT);
            }
            stat(c, i(a, 4)?, false)?;
        }
        "fd_filestat_get" => {
            let fd = i(a, 0)?;
            if fd != 3 && !c.data().cursors.contains_key(&fd) {
                return Ok(BADF);
            }
            stat(c, i(a, 1)?, fd == 3)?;
        }
        "fd_fdstat_get" => {
            let fd = i(a, 0)?;
            let mut bytes = [0; 24];
            if fd == 1 || fd == 2 {
                bytes[0] = 2;
                bytes[8..16].copy_from_slice(&(1u64 << 6).to_le_bytes());
            } else if fd == 3 && c.data().source.is_some() {
                bytes[0] = 3;
                bytes[8..16].copy_from_slice(&((1u64 << 13) | (1u64 << 18)).to_le_bytes());
            } else if c.data().cursors.contains_key(&fd) {
                bytes[0] = 4;
                bytes[8..16].copy_from_slice(
                    &((1u64 << 1) | (1u64 << 2) | (1u64 << 5) | (1u64 << 21)).to_le_bytes(),
                );
            } else {
                return Ok(BADF);
            };
            write(c, i(a, 1)?, &bytes)?;
        }
        "fd_close" => {
            if c.data_mut().cursors.remove(&i(a, 0)?).is_none() {
                return Ok(BADF);
            };
        }
        "fd_fdstat_set_flags" => {
            if i(a, 1)? != 0 || !c.data().cursors.contains_key(&i(a, 0)?) {
                return Ok(NOTCAPABLE);
            };
        }
        "fd_seek" => {
            let fd = i(a, 0)?;
            let Some(old) = c.data().cursors.get(&fd).copied() else {
                return Ok(BADF);
            };
            let base = match i(a, 2)? {
                0 => 0,
                1 => old,
                2 => c.data().source_size,
                _ => return Ok(INVAL),
            };
            let target = i128::from(base) + i128::from(l(a, 1)?);
            let Ok(target) = u64::try_from(target) else {
                return Ok(INVAL);
            };
            c.data_mut().cursors.insert(fd, target);
            write(c, i(a, 3)?, &target.to_le_bytes())?;
        }
        "fd_read" | "fd_pread" | "fd_write" => {
            let fd = i(a, 0)?;
            let count = i(a, 2)? as u32 as usize;
            if count > 1024 {
                return Err(Error::msg("parser iovec count exceeds bound"));
            };
            let writing = name == "fd_write";
            if writing && fd != 1 && fd != 2 {
                return Ok(BADF);
            };
            let mut offset = if writing {
                0
            } else if name == "fd_pread" {
                l(a, 3)? as u64
            } else {
                match c.data().cursors.get(&fd) {
                    Some(v) => *v,
                    None => return Ok(BADF),
                }
            };
            if !writing && !c.data().cursors.contains_key(&fd) {
                return Ok(BADF);
            };
            let mut total = 0usize;
            for n in 0..count {
                check(c.data())?;
                let v = ptr(i(a, 1)?, n * 8)?;
                let dest = u32_at(c, v)? as i32;
                let len = (u32_at(c, ptr(v, 4)?)? as usize).min(IO_LIMIT - total);
                if len == 0 {
                    break;
                };
                if writing {
                    let bytes = read(c, dest, len)?;
                    let limit = if fd == 1 {
                        c.data().stdout_limit
                    } else {
                        c.data().stderr_limit
                    };
                    let output = if fd == 1 {
                        &mut c.data_mut().stdout
                    } else {
                        &mut c.data_mut().stderr
                    };
                    if output.len().saturating_add(len) > limit {
                        return Err(Error::msg("parser output exceeds bound"));
                    };
                    output.extend_from_slice(&bytes);
                    total += len;
                } else {
                    let source = c
                        .data()
                        .source
                        .as_ref()
                        .ok_or_else(|| Error::msg("parser has no source"))?;
                    let mut bytes = vec![0; len];
                    let got = source
                        .read_at(&mut bytes, offset)
                        .map_err(|e| Error::msg(e.to_string()))?;
                    write(c, dest, &bytes[..got])?;
                    total += got;
                    offset = offset
                        .checked_add(got as u64)
                        .ok_or_else(|| Error::msg("source offset overflow"))?;
                    if got < len {
                        break;
                    };
                }
            }
            if name == "fd_read" {
                c.data_mut().cursors.insert(fd, offset);
            }
            write(
                c,
                i(a, if name == "fd_pread" { 4 } else { 3 })?,
                &(total as u32).to_le_bytes(),
            )?;
        }
        _ => return Ok(NOTCAPABLE),
    }
    Ok(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn parser(source: &str) -> Parser {
        Parser::new(&wat::parse_str(source).expect("compile regression image"))
            .expect("admit image")
    }
    fn run(parser: &Parser, source: Option<Arc<File>>, limit: usize) -> Result<Output, String> {
        parser.run(
            source,
            Vec::new(),
            (limit, 16 * 1024),
            Instant::now() + std::time::Duration::from_secs(1),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        )
    }
    #[test]
    fn parser_imports_cannot_acquire_network_process_or_host_memory() {
        for source in [
            r#"(module (import "wasi_snapshot_preview1" "sock_open" (func)))"#,
            r#"(module (import "wasi_snapshot_preview1" "proc_spawn" (func)))"#,
            r#"(module (import "host" "memory" (memory 1)))"#,
        ] {
            assert!(Parser::new(&wat::parse_str(source).expect("image")).is_err());
        }
    }
    #[test]
    fn module_start_cannot_run_outside_the_execution_owner() {
        let image = wat::parse_str("(module (func $f (loop $again (br $again))) (start $f))")
            .expect("image");
        assert!(Parser::new(&image).is_err());
    }

    #[test]
    fn standard_simd_is_available_but_relaxed_simd_is_refused() {
        let p = parser("(module (func (export \"_start\") (drop (v128.const i32x4 0 0 0 0))))");
        assert!(run(&p, None, 32).is_ok());
        let relaxed = wat::parse_str(
            "(module (func (export \"_start\") (drop (i8x16.relaxed_swizzle
              (v128.const i32x4 0 0 0 0) (v128.const i32x4 0 0 0 0)))))",
        )
        .expect("compile relaxed SIMD regression image");
        assert!(Parser::new(&relaxed).is_err());
    }
    #[test]
    fn parser_reads_only_the_held_source_without_changing_its_offset() {
        use std::io::{Seek, SeekFrom, Write};
        let mut file = tempfile::tempfile().expect("source");
        file.write_all(b"held-source").expect("write source");
        file.seek(SeekFrom::Start(7)).expect("producer offset");
        let file = Arc::new(file);
        let p = parser(
            r#"(module
            (import "wasi_snapshot_preview1" "path_open" (func $open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_read" (func $read (param i32 i32 i32 i32) (result i32)))
            (import "wasi_snapshot_preview1" "fd_write" (func $write (param i32 i32 i32 i32) (result i32)))
            (memory (export "memory") 1)
            (data (i32.const 0) "dev/fd/3")
            (data (i32.const 24) "\40\00\00\00\04\00\00\00")
            (func (export "_start")
                (drop (call $open (i32.const 3) (i32.const 0) (i32.const 0) (i32.const 8) (i32.const 0) (i64.const 2) (i64.const 0) (i32.const 0) (i32.const 32)))
                (drop (call $read (i32.load (i32.const 32)) (i32.const 24) (i32.const 1) (i32.const 36)))
                (drop (call $write (i32.const 1) (i32.const 24) (i32.const 1) (i32.const 36)))))"#,
        );
        let result = run(&p, Some(Arc::clone(&file)), 32).expect("held-source result");
        assert_eq!(result.stdout, b"held");
        assert_eq!((&*file).stream_position().expect("offset"), 7);
    }
    #[test]
    fn virtual_directory_does_not_grant_other_files_or_write_access() {
        let source = Arc::new(tempfile::tempfile().expect("source"));
        for (path, rights, expected) in [("secret!!", 2, NOENT), ("dev/fd/3", 64, NOTCAPABLE)] {
            let p = parser(&format!(
                r#"(module
                (import "wasi_snapshot_preview1" "path_open" (func $open (param i32 i32 i32 i32 i32 i64 i64 i32 i32) (result i32)))
                (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
                (memory (export "memory") 1) (data (i32.const 0) "{path}")
                (func (export "_start") (call $exit (call $open (i32.const 3) (i32.const 0) (i32.const 0) (i32.const 8) (i32.const 0) (i64.const {rights}) (i64.const 0) (i32.const 0) (i32.const 32)))))"#
            ));
            assert_eq!(
                run(&p, Some(Arc::clone(&source)), 32)
                    .expect("denial result")
                    .status,
                expected
            );
        }
    }
    #[test]
    fn endless_guest_code_observes_the_existing_deadline() {
        let p = parser("(module (func (export \"_start\") (loop $again (br $again))))");
        let deadline = Instant::now() + std::time::Duration::from_millis(1);
        let result = p.run(
            None,
            Vec::new(),
            (32, 16 * 1024),
            deadline,
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        assert_eq!(result.err().as_deref(), Some("parser deadline"));
    }
}
