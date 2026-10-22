//! Fixed, non-exporting reader ownership for finite software-VOD pipe adapters.
//!
//! This primitive only ties reader lifetime to the actual standard pipe handle.
//! It does not prove task, process, descendant, or whole-resource closure.
use std::io;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use tokio::io::{AsyncRead, ReadBuf};
use tokio::process::{ChildStderr, ChildStdout};

#[derive(Debug)]
enum NativePipe {
    Stdout(ChildStdout),
    Stderr(ChildStderr),
}

#[derive(Debug)]
struct State {
    pipe: Mutex<Option<NativePipe>>,
    reader_alive: AtomicBool,
}

/// Move-only async reader. It exposes no descriptor, handle, or clone operation.
pub struct FinitePipeReader {
    state: Arc<State>,
}

impl AsyncRead for FinitePipeReader {
    fn poll_read(
        self: Pin<&mut Self>,
        context: &mut Context<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let mut state = match self.state.pipe.lock() {
            Ok(state) => state,
            Err(_) => {
                return Poll::Ready(Err(io::Error::other(
                    "finite pipe state lock poisoned; native pipe retained",
                )))
            }
        };
        let Some(pipe) = state.as_mut() else {
            return Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "finite pipe was already closed by its core owner",
            )));
        };
        match pipe {
            NativePipe::Stdout(pipe) => Pin::new(pipe).poll_read(context, buffer),
            NativePipe::Stderr(pipe) => Pin::new(pipe).poll_read(context, buffer),
        }
    }
}

impl Drop for FinitePipeReader {
    fn drop(&mut self) {
        self.state.reader_alive.store(false, Ordering::Release);
    }
}

/// Non-exported core record for the same pipe state as its reader.
pub(super) struct PipeRecord {
    state: Arc<State>,
}

impl PipeRecord {
    /// Close only after every reader owner has actually dropped its reader.
    /// A poisoned lock keeps the original pipe in the retained state.
    pub(super) fn close_after_reader_drop(&self) -> io::Result<()> {
        if self.state.reader_alive.load(Ordering::Acquire) {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "finite pipe reader is still alive",
            ));
        }
        let mut pipe =
            self.state.pipe.lock().map_err(|_| {
                io::Error::other("finite pipe lock poisoned; pipe remains retained")
            })?;
        drop(pipe.take());
        Ok(())
    }
}

/// The two actual child output pipes, exposed only as async readers.
pub struct FiniteRootPipes {
    pub stdout: FinitePipeReader,
    pub stderr: FinitePipeReader,
}

/// Move the actual child pipes into paired reader/record owners.
pub(super) fn prepare(
    stdout: ChildStdout,
    stderr: ChildStderr,
) -> ([PipeRecord; 2], FiniteRootPipes) {
    let stdout = Arc::new(State {
        pipe: Mutex::new(Some(NativePipe::Stdout(stdout))),
        reader_alive: AtomicBool::new(true),
    });
    let stderr = Arc::new(State {
        pipe: Mutex::new(Some(NativePipe::Stderr(stderr))),
        reader_alive: AtomicBool::new(true),
    });
    (
        [
            PipeRecord {
                state: Arc::clone(&stdout),
            },
            PipeRecord {
                state: Arc::clone(&stderr),
            },
        ],
        FiniteRootPipes {
            stdout: FinitePipeReader { state: stdout },
            stderr: FinitePipeReader { state: stderr },
        },
    )
}

/// Exact retained Rust allocation accounting, excluding OS and Tokio baselines.
pub(super) fn resident_memory_bytes() -> io::Result<usize> {
    let per_pipe = std::mem::size_of::<State>()
        .checked_add(
            2usize
                .checked_mul(std::mem::size_of::<usize>())
                .ok_or_else(|| io::Error::other("finite pipe memory accounting overflow"))?,
        )
        .ok_or_else(|| io::Error::other("finite pipe memory accounting overflow"))?;
    2usize
        .checked_mul(per_pipe)
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<[PipeRecord; 2]>()))
        .and_then(|bytes| bytes.checked_add(std::mem::size_of::<FiniteRootPipes>()))
        .ok_or_else(|| io::Error::other("finite pipe memory accounting overflow"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::process::Stdio;
    use tokio::io::AsyncReadExt;

    async fn child_pipes() -> (tokio::process::Child, ChildStdout, ChildStderr) {
        let mut child = tokio::process::Command::new("/bin/sh")
            .args(["-c", "printf stdout-data; printf stderr-data >&2"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn fixture shell");
        let stdout = child.stdout.take().expect("fixture stdout");
        let stderr = child.stderr.take().expect("fixture stderr");
        (child, stdout, stderr)
    }

    #[tokio::test]
    async fn detached_reader_blocks_close_until_its_task_is_joined() {
        let (mut child, stdout, stderr) = child_pipes().await;
        let (records, pipes) = prepare(stdout, stderr);
        child.wait().await.expect("wait actual fixture child");
        let reader = pipes.stdout;
        drop(pipes.stderr);
        let task = tokio::spawn(async move {
            let _reader = reader;
            std::future::pending::<()>().await;
        });
        assert_eq!(
            records[0].close_after_reader_drop().unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );
        task.abort();
        assert!(task.await.unwrap_err().is_cancelled());
        records[0]
            .close_after_reader_drop()
            .expect("joined task dropped the actual reader");
        records[1]
            .close_after_reader_drop()
            .expect("unmoved stderr reader can close after owner drop");
    }

    #[tokio::test]
    async fn readers_consume_actual_pipes_and_records_close_after_drop() {
        let (mut child, stdout, stderr) = child_pipes().await;
        let (records, pipes) = prepare(stdout, stderr);
        let (mut out, mut err) = (pipes.stdout, pipes.stderr);
        let (mut out_bytes, mut err_bytes) = (Vec::new(), Vec::new());
        let (out_result, err_result, status) = tokio::join!(
            out.read_to_end(&mut out_bytes),
            err.read_to_end(&mut err_bytes),
            child.wait(),
        );
        out_result.expect("read actual stdout");
        err_result.expect("read actual stderr");
        status.expect("wait actual fixture child");
        assert_eq!(out_bytes, b"stdout-data");
        assert_eq!(err_bytes, b"stderr-data");

        drop(out);
        drop(err);
        records[0]
            .close_after_reader_drop()
            .expect("stdout closes only after its reader drops");
        records[1]
            .close_after_reader_drop()
            .expect("stderr closes only after its reader drops");
    }
}
