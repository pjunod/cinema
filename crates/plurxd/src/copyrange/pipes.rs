//! One owned FFmpeg input, AV stdout, canonical MP4 and source packet clock.
//! Every pipe task is bounded and joined before the registered writer ack.
//! The generation's owner is typed: Source playback or an Analysis phase.
use crate::{
    prodrun::{ProducerRegistration, ProducerSlot, ProducerWriters},
    source_generation::{SourceGenerationDispatch, SourceRenditionOwners},
    source_input::SourceInput,
    transcode::source_actor::SourceGenerationAuthority,
};
use plurx_core::store::analysis_coordination::AnalysisPhysicalPhase;
use std::{path::Path, sync::Arc};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Who owns one copy generation's physical work. The recipe, pipes, parsers
/// and reap are the same code for both; only admission and custody differ.
pub(crate) enum CopyGenerationOwner<'a> {
    /// Shared Source playback: the rendition's retained admission permit, its
    /// exact generation dispatch and a current media-serving authority.
    Source {
        owners: &'a SourceRenditionOwners,
        dispatch: SourceGenerationDispatch,
        authority: SourceGenerationAuthority,
    },
    /// An exact Analysis physical phase whose custody is already active. Its
    /// Input reads under that phase's byte budget. It holds no admission
    /// permit and no Source grant, and nothing it produces may be served.
    Analysis { phase: &'a AnalysisPhysicalPhase },
}
impl CopyGenerationOwner<'_> {
    /// Refusals that need no child. The Input is the caller's to close.
    fn validate_before_spawn(&self, input: &SourceInput) -> Result<(), String> {
        match self {
            Self::Source { authority, .. } => authority.validate_before_spawn(),
            Self::Analysis { phase } => {
                // A foreground Input carries a viewer's custody and observer;
                // an Analysis phase may only read under its own budget.
                if input.foreground_observer().is_some()
                    || !matches!(
                        &phase.custody.owner,
                        plurx_core::store::background_jobs_resources::BackgroundSourceOwner::Analysis {
                            request_id,
                            node_id,
                            fence,
                        } if request_id == &phase.claim.request_id
                            && node_id == &phase.claim.node_id
                            && *fence == phase.claim.fence
                    )
                {
                    return Err("copy_analysis_owner_invalid".into());
                }
                Ok(())
            }
        }
    }
}

const PIPE_CHUNK: usize = 32 * 1024;
const PIPE_CHANNEL: usize = 8;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CopyPipeKind {
    Av,
    Canonical,
    Clock,
    Diagnostic,
}
pub(crate) enum CopyPipeEvent {
    Data(CopyPipeKind, Vec<u8>),
    Eof(CopyPipeKind),
    Failed(CopyPipeKind, String),
}
pub(crate) struct CopyPipeReaders {
    receiver: tokio::sync::mpsc::Receiver<CopyPipeEvent>,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    eof: u8,
}
impl CopyPipeReaders {
    pub(crate) async fn next(&mut self) -> Option<CopyPipeEvent> {
        let event = self.receiver.recv().await?;
        if let CopyPipeEvent::Eof(kind) = &event {
            self.eof |= 1u8 << (*kind as u8);
        }
        Some(event)
    }
    pub(crate) fn all_eof(&self) -> bool {
        self.eof == 15
    }
    /// Closing the queue drops every output read end after an in-flight read
    /// returns. The generation owner must request child retirement first on
    /// failure; successful EOF paths have already drained all four streams.
    pub(crate) async fn join(mut self) -> Result<(), String> {
        self.receiver.close();
        while self.receiver.try_recv().is_ok() {}
        let mut failed = false;
        for task in self.tasks {
            if task.await.is_err() {
                failed = true;
            }
        }
        if failed {
            Err("copy_output_reader_panicked".into())
        } else {
            Ok(())
        }
    }
}
fn reader<R: AsyncRead + Unpin + Send + 'static>(
    mut input: R,
    kind: CopyPipeKind,
    sender: tokio::sync::mpsc::Sender<CopyPipeEvent>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            let mut buffer = vec![0u8; PIPE_CHUNK];
            let event = match input.read(&mut buffer).await {
                Ok(0) => CopyPipeEvent::Eof(kind),
                Ok(count) => {
                    buffer.truncate(count);
                    CopyPipeEvent::Data(kind, buffer)
                }
                Err(_) => CopyPipeEvent::Failed(kind, "copy_output_read_failed".into()),
            };
            let done = !matches!(event, CopyPipeEvent::Data(..));
            if sender.send(event).await.is_err() || done {
                return;
            }
        }
    })
}
fn output_pair() -> Result<(std::fs::File, tokio::net::UnixStream), String> {
    let (read, write) =
        std::os::unix::net::UnixStream::pair().map_err(|_| "copy_output_pair_failed".to_owned())?;
    read.set_nonblocking(true)
        .map_err(|_| "copy_output_pair_nonblocking".to_owned())?;
    let read = tokio::net::UnixStream::from_std(read)
        .map_err(|_| "copy_output_pair_runtime".to_owned())?;
    let descriptor: std::os::fd::OwnedFd = write.into();
    Ok((std::fs::File::from(descriptor), read))
}

pub(crate) struct RunningCopyGeneration {
    pub(crate) slot: Arc<ProducerSlot>,
    pub(crate) registration: ProducerRegistration,
    writers: Option<ProducerWriters>,
    outputs_joined: bool,
    pub(crate) pipes: Option<CopyPipeReaders>,
}
impl RunningCopyGeneration {
    pub(crate) async fn join_completed_outputs(&mut self) -> Result<(), String> {
        if !self.pipes.as_ref().is_some_and(CopyPipeReaders::all_eof) {
            return Err("copy_output_incomplete".into());
        }
        self.pipes
            .take()
            .ok_or("copy_output_readers_missing")?
            .join()
            .await?;
        self.outputs_joined = true;
        Ok(())
    }

    /// Consume after parser/manifest work. Cancellation first kills through
    /// the same exact-generation owner; all pipe tasks precede writer ack,
    /// which precedes held Input/job/admission release and final reap receipt.
    pub(crate) async fn settle(
        mut self,
        success: bool,
    ) -> Result<crate::prodrun::ConfirmedProducerReap, String> {
        let complete = success
            && (self.outputs_joined || self.pipes.as_ref().is_some_and(CopyPipeReaders::all_eof));
        let transition = if complete {
            self.slot
                .request_registered_completion(&self.registration)
                .await
        } else {
            self.slot
                .request_registered_retirement(&self.registration)
                .await
        };
        if transition.is_err() {
            let _ = self
                .slot
                .request_registered_retirement(&self.registration)
                .await;
        }
        let joined = match self.pipes.take() {
            Some(pipes) => pipes.join().await,
            None => Ok(()),
        };
        if let Some(writers) = self.writers.take() {
            writers.settled();
        }
        let receipt = self.registration.wait_confirmed_reap().await;
        joined?;
        if success && !complete {
            return Err("copy_output_incomplete".into());
        }
        Ok(receipt)
    }
}

pub(crate) async fn spawn_source_generation(
    input: SourceInput,
    source_path: &Path,
    args: &[String],
    runtime_cache: &Path,
    owner: CopyGenerationOwner<'_>,
    slot: Arc<ProducerSlot>,
) -> Result<RunningCopyGeneration, String> {
    // Pair creation is fallible before any child exists. Retain and explicitly
    // close/join the input on every refusal rather than dropping its waiter.
    let pairs = output_pair().and_then(|canonical| output_pair().map(|clock| (canonical, clock)));
    let ((canonical_write, canonical), (clock_write, clock)) = match pairs {
        Ok(pairs) => pairs,
        Err(error) => {
            let _ = input.close_and_wait().await;
            return Err(error);
        }
    };
    if let Err(error) = owner.validate_before_spawn(&input) {
        let _ = input.close_and_wait().await;
        return Err(error);
    }
    let args = crate::source_input::source_args(args, source_path, &[3], Some(input.url()));
    if args.iter().filter(|arg| arg.as_str() == "-i").count() != 1
        || args
            .windows(2)
            .any(|pair| pair[0] == "-i" && pair[1] != input.url())
    {
        let _ = input.close_and_wait().await;
        return Err("copy_requires_one_input".into());
    }
    let descriptors = crate::producer_spawn::Descriptors::from_files(None, None, None, false)
        .with_copy_measurements(&canonical_write, &clock_write);
    let (permit, work): (Option<Box<dyn Send>>, _) = match &owner {
        CopyGenerationOwner::Source { owners, .. } => match owners.retained_initial_permit() {
            Some(permit) => (
                Some(Box::new(permit)),
                crate::process_control::ChildWork::realtime("Source observed copy"),
            ),
            None => {
                let _ = input.close_and_wait().await;
                return Err("copy_source_admission_missing".into());
            }
        },
        CopyGenerationOwner::Analysis { .. } => (
            None,
            crate::process_control::ChildWork::background("Analysis observed copy"),
        ),
    };
    let spawned = crate::producer_spawn::spawn(
        Path::new(&crate::ffmpeg::ffmpeg_bin()),
        &args,
        crate::producer_spawn::SpawnOptions {
            runtime_cache,
            progress: crate::producer_spawn::Progress::None,
            descriptors,
            env: &[],
            work,
        },
    );
    let spawned = match spawned {
        Ok(spawned) => spawned,
        Err(error) => {
            let _ = input.close_and_wait().await;
            return Err(error);
        }
    };
    drop(canonical_write);
    drop(clock_write);
    let (registration, writers) = slot
        .attach_registered_source_job_owned(
            spawned.child,
            spawned.child_job,
            0,
            permit,
            Some(input),
        )
        .await;
    if let CopyGenerationOwner::Source {
        owners, dispatch, ..
    } = owner
    {
        owners.registered(&dispatch, &registration);
    }
    let (sender, receiver) = tokio::sync::mpsc::channel(PIPE_CHANNEL);
    let tasks = vec![
        reader(spawned.stdout, CopyPipeKind::Av, sender.clone()),
        reader(canonical, CopyPipeKind::Canonical, sender.clone()),
        reader(clock, CopyPipeKind::Clock, sender.clone()),
        reader(spawned.stderr, CopyPipeKind::Diagnostic, sender),
    ];
    Ok(RunningCopyGeneration {
        slot,
        registration,
        writers: Some(writers),
        outputs_joined: false,
        pipes: Some(CopyPipeReaders {
            receiver,
            tasks,
            eof: 0,
        }),
    })
}
