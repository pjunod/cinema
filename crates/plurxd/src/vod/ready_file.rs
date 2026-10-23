//! Opaque response ownership for local and counted finite VOD files.
use bytes::Bytes;
use futures_util::Stream;
#[cfg(unix)]
use plurx_core::adaptive_resources::runtime::finite_vod::{
    FiniteFile, FiniteOutput, FiniteReader, FiniteVodError,
};
use std::fmt;
use std::io;
use std::pin::Pin;
use std::time::Instant;
use tokio::io::{AsyncReadExt, AsyncSeekExt};

pub(crate) type MediaByteStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

/// A selected response object. The finite variant retains only a counted
/// selection and file key; it never exports the native file or its payload.
pub(crate) enum ReadyFile {
    Local(tokio::fs::File),
    #[cfg(unix)]
    Finite {
        output: FiniteOutput,
        file: FiniteFile,
        len: u64,
    },
}

impl fmt::Debug for ReadyFile {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Local(_) => formatter.write_str("ReadyFile::Local(..)"),
            #[cfg(unix)]
            Self::Finite { len, .. } => formatter
                .debug_struct("ReadyFile::Finite")
                .field("len", len)
                .finish_non_exhaustive(),
        }
    }
}

impl From<tokio::fs::File> for ReadyFile {
    fn from(file: tokio::fs::File) -> Self {
        Self::Local(file)
    }
}

impl ReadyFile {
    pub(crate) async fn read_all_bounded(
        &mut self,
        limit: u64,
        deadline: Instant,
    ) -> io::Result<Vec<u8>> {
        let len = match self {
            Self::Local(file) => timeout(deadline, file.metadata()).await??.len(),
            #[cfg(unix)]
            Self::Finite { len, .. } => *len,
        };
        if len > limit {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "response exceeds bounded read limit",
            ));
        }
        let size = usize::try_from(len).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "response length is not addressable",
            )
        })?;
        match self {
            Self::Local(file) => {
                timeout(deadline, file.seek(std::io::SeekFrom::Start(0))).await??;
                let mut bytes = vec![0; size];
                timeout(deadline, file.read_exact(&mut bytes)).await??;
                Ok(bytes)
            }
            #[cfg(unix)]
            Self::Finite { output, file, len } => {
                if *len == 0 {
                    return Ok(Vec::new());
                }
                let mut reader = timeout(
                    deadline,
                    output.prepare_reader(*file, 0, *len, limit, deadline),
                )
                .await?
                .map_err(finite_error)?;
                let result = read_finite_all(&mut reader, size, deadline).await;
                let closed = reader.close(deadline).await.map_err(finite_error);
                match (result, closed) {
                    (Err(error), _) => Err(error),
                    (Ok(_), Err(error)) => Err(error),
                    (Ok(bytes), Ok(())) => Ok(bytes),
                }
            }
        }
    }

    pub(crate) async fn into_range_stream(
        self,
        start: u64,
        end_exclusive: u64,
        chunk_capacity: usize,
        deadline: Instant,
    ) -> io::Result<MediaByteStream> {
        let len = match &self {
            Self::Local(file) => timeout(deadline, file.metadata()).await??.len(),
            #[cfg(unix)]
            Self::Finite { len, .. } => *len,
        };
        if start > end_exclusive || end_exclusive > len || chunk_capacity == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid fixed-file range",
            ));
        }
        let chunk_capacity_u64 = u64::try_from(chunk_capacity)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "chunk capacity overflow"))?;
        let remaining = end_exclusive - start;
        if remaining == 0 {
            return Ok(Box::pin(futures_util::stream::empty()));
        }
        let source = match self {
            Self::Local(mut file) => {
                timeout(deadline, file.seek(std::io::SeekFrom::Start(start))).await??;
                RangeSource::Local {
                    file,
                    remaining,
                    deadline,
                }
            }
            #[cfg(unix)]
            Self::Finite { output, file, .. } => {
                let reader = timeout(
                    deadline,
                    output.prepare_reader(file, start, end_exclusive, chunk_capacity_u64, deadline),
                )
                .await?
                .map_err(finite_error)?;
                RangeSource::Finite {
                    reader,
                    remaining,
                    deadline,
                }
            }
        };
        let stream = futures_util::stream::try_unfold(source, move |source| {
            next_range_chunk(source, chunk_capacity_u64)
        });
        Ok(Box::pin(stream))
    }
}

enum RangeSource {
    Local {
        file: tokio::fs::File,
        remaining: u64,
        deadline: Instant,
    },
    #[cfg(unix)]
    Finite {
        reader: FiniteReader,
        remaining: u64,
        deadline: Instant,
    },
}

async fn next_range_chunk(
    source: RangeSource,
    chunk_capacity: u64,
) -> io::Result<Option<(Bytes, RangeSource)>> {
    match source {
        RangeSource::Local {
            mut file,
            mut remaining,
            deadline,
        } => {
            if remaining == 0 {
                return Ok(None);
            }
            let count = remaining.min(chunk_capacity) as usize;
            let mut bytes = vec![0; count];
            timeout(deadline, file.read_exact(&mut bytes)).await??;
            remaining -= count as u64;
            Ok(Some((
                Bytes::from(bytes),
                RangeSource::Local {
                    file,
                    remaining,
                    deadline,
                },
            )))
        }
        #[cfg(unix)]
        RangeSource::Finite {
            reader,
            mut remaining,
            deadline,
        } => {
            let Some(chunk) = reader.next_chunk(deadline).await.map_err(finite_error)? else {
                if remaining != 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "short finite range",
                    ));
                }
                return Ok(None);
            };
            let count = chunk.as_ref().len();
            if count == 0 || count as u64 > chunk_capacity || count as u64 > remaining {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "invalid finite chunk length",
                ));
            }
            remaining -= count as u64;
            Ok(Some((
                Bytes::from_owner(chunk),
                RangeSource::Finite {
                    reader,
                    remaining,
                    deadline,
                },
            )))
        }
    }
}

#[cfg(unix)]
async fn read_finite_all(
    reader: &mut FiniteReader,
    size: usize,
    deadline: Instant,
) -> io::Result<Vec<u8>> {
    let mut result = Vec::with_capacity(size);
    while result.len() < size {
        let Some(chunk) = reader.next_chunk(deadline).await.map_err(finite_error)? else {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "short finite file",
            ));
        };
        let remaining = size - result.len();
        if chunk.as_ref().is_empty() || chunk.as_ref().len() > remaining {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "invalid finite chunk length",
            ));
        }
        result.extend_from_slice(chunk.as_ref());
    }
    Ok(result)
}

async fn timeout<T>(
    deadline: Instant,
    future: impl std::future::Future<Output = T>,
) -> io::Result<T> {
    tokio::time::timeout_at(tokio::time::Instant::from_std(deadline), future)
        .await
        .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "response I/O deadline"))
}

#[cfg(unix)]
fn finite_error(error: FiniteVodError) -> io::Error {
    error.into_io_error()
}
