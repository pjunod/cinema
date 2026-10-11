// Actual registered segment producer ownership. No Source viewer is dispatched.
struct SourceLifetimeWait {
    pause: Arc<crate::seam_hooks::AsyncPause>,
    retry: Arc<crate::seam_hooks::AsyncPause>,
    fail_once: AtomicBool,
}
impl crate::prodrun::ProducerReapHooks for SourceLifetimeWait {
    fn wait<'a>(
        &'a self,
        child: &'a mut tokio::process::Child,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::io::Result<std::process::ExitStatus>> + Send + 'a,
        >,
    > {
        Box::pin(async move {
            if self.fail_once.swap(false, AcqRel) {
                self.pause.hold().await;
                return Err(std::io::Error::other("injected producer wait failure"));
            }
            self.retry.hold().await;
            child.wait().await
        })
    }
}

#[cfg(unix)]
#[tokio::test]
async fn source_registered_producer_retains_real_permit_until_wait_and_writers() {
    let base = crate::test_tempdir().expect("fixture");
    let (_, encoding) = encoded_fixture(base.path()).await;
    let permit = encoding
        .try_permit()
        .await
        .expect("actual VOD EncodePermit");
    let used = encoding.admissions.software_in_use();
    assert!(used > 0);
    let slot = Arc::new(crate::prodrun::ProducerSlot::new());
    let pause = crate::seam_hooks::AsyncPause::new("actual registered producer wait");
    let retry_pause = crate::seam_hooks::AsyncPause::new("retained producer wait retry");
    slot.set_reap_hooks(Arc::new(SourceLifetimeWait {
        pause: Arc::clone(&pause),
        retry: Arc::clone(&retry_pause),
        fail_once: AtomicBool::new(true),
    }))
    .await;
    let (child, job) = crate::process_control::spawn_job_owned(
        tokio::process::Command::new("sleep").arg("60"),
        crate::process_control::ChildWork::realtime("Source lifetime fixture"),
    )
    .expect("actual owned child");
    let pid = child.id().expect("pid");
    let (generation, writers) = slot
        .attach_registered_job_owned(child, job, 0, Some(Box::new(permit)))
        .await;
    let independent = tokio::spawn({
        let generation = generation.clone();
        async move { generation.wait_confirmed_reap().await }
    });
    let waiter = tokio::spawn({
        let slot = Arc::clone(&slot);
        async move {
            slot.perform(
                Step::Terminate {
                    why: Termination::Idle,
                },
                || {},
            )
            .await
        }
    });
    let held = pause.reached().await;
    waiter.abort();
    let _ = waiter.await;
    assert_eq!(encoding.admissions.software_in_use(), used);
    assert!(generation.confirmed_reap().is_none());
    assert!(
        !independent.is_finished(),
        "association cannot settle before actual process/writers"
    );
    held.release();
    let retry = retry_pause.reached().await;
    assert_eq!(
        encoding.admissions.software_in_use(),
        used,
        "wait error retains actual permit"
    );
    assert!(generation.confirmed_reap().is_none());
    retry.release();
    tokio::time::sleep(Duration::from_millis(25)).await;
    assert_eq!(
        encoding.admissions.software_in_use(),
        used,
        "confirmed child wait does not settle writers"
    );
    assert!(generation.confirmed_reap().is_none());
    writers.settled();
    let receipt = tokio::time::timeout(
        Duration::from_secs(5),
        slot.wait_registered_retirement(&generation),
    )
    .await
    .expect("bounded retirement")
    .expect("actual receipt");
    assert!(receipt.matches(&generation));
    assert!(independent
        .await
        .expect("independent generation association")
        .matches(&generation));
    assert_eq!(encoding.admissions.software_in_use(), 0);
    assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );

    let (child, job) = crate::process_control::spawn_job_owned(
        tokio::process::Command::new("sleep").arg("60"),
        crate::process_control::ChildWork::realtime("new producer lifetime fixture"),
    )
    .expect("new actual child");
    let new_pid = child.id().expect("new pid");
    let (new_generation, new_writers) = slot.attach_registered_job_owned(child, job, 0, None).await;
    assert!(!receipt.matches(&new_generation));
    assert!(!slot
        .request_registered_retirement(&generation)
        .await
        .expect("old registered receipt does not kill successor"));
    assert_eq!(unsafe { libc::kill(new_pid as libc::pid_t, 0) }, 0);
    // Install the production wait seam before retiring the exact successor.
    slot.set_reap_hooks(Arc::new(ActualSourceLifetimeWait))
        .await;
    new_writers.settled();
    slot.request_registered_retirement(&new_generation)
        .await
        .expect("new exact retirement");
    slot.wait_registered_retirement(&new_generation)
        .await
        .expect("new receipt");
}

struct ActualSourceLifetimeWait;
impl crate::prodrun::ProducerReapHooks for ActualSourceLifetimeWait {
    fn wait<'a>(
        &'a self,
        child: &'a mut tokio::process::Child,
    ) -> std::pin::Pin<
        Box<
            dyn std::future::Future<Output = std::io::Result<std::process::ExitStatus>> + Send + 'a,
        >,
    > {
        Box::pin(child.wait())
    }
}

struct SourceRegistrationPause(Arc<crate::seam_hooks::AsyncPause>);
impl RenditionHooks for SourceRegistrationPause {
    fn stopped_poll_armed(&self) {}
    fn stopped_poll_fired(&self) {}
    fn before_producer_registration(&self) -> crate::seam_hooks::HookFuture<'_> {
        Box::pin(async move {
            self.0.hold().await;
        })
    }
}

/// The actual VOD generation path, including FFmpeg/ChildJob/EncodePermit.
/// No Shared principal, admission claim or HTTP Source worker is enabled here.
#[cfg(unix)]
#[tokio::test]
async fn source_generation_registration_close_and_cancel_retain_actual_resources() {
    for cancel_waiter in [false, true] {
        let base = crate::test_tempdir().expect("actual generation fixture");
        let (file, encoding) = encoded_fixture(base.path()).await;
        let serve = bare_serve(&base.path().join("renditions"));
        let mut rendition = serve
            .shared
            .build_rendition(
                "owned-registration",
                None,
                Recipe {
                    // Physical legacy-AAC fixture, no candidate or retained artifact.
                    audio_delivery: None,
                    measured_candidate: None,
                    retained_logical: None,
                    file,
                    audio_index: None,
                    aac: true,
                    video: CopyVideoOptions::new(false, false),
                    source_object_version: Some(encoding.source_object_version.clone()),
                    cluster_cache_key: None,
                    encoding: Some(Arc::clone(&encoding)),
                },
                encoding.grid.plan(96_000, 428_000),
                &settings(),
            )
            .await
            .expect("actual encoded rendition");
        let registration_pause =
            crate::seam_hooks::AsyncPause::new("owned registration after actual spawn");
        Arc::get_mut(&mut rendition)
            .expect("unpublished rendition")
            .hooks = Box::new(SourceRegistrationPause(Arc::clone(&registration_pause)));
        let failed_wait = crate::seam_hooks::AsyncPause::new("owned postspawn wait failure");
        let retry_wait = crate::seam_hooks::AsyncPause::new("owned postspawn wait retry");
        rendition
            .slot
            .set_reap_hooks(Arc::new(SourceLifetimeWait {
                pause: Arc::clone(&failed_wait),
                retry: Arc::clone(&retry_wait),
                fail_once: AtomicBool::new(true),
            }))
            .await;
        let permit = encoding.try_permit().await.expect("actual physical permit");
        let used = encoding.admissions.software_in_use();
        assert!(used > 0);
        let caller = tokio::spawn({
            let shared = Arc::clone(&serve.shared);
            let rendition = Arc::clone(&rendition);
            async move { spawn_generation(&shared, &rendition, 0, Some(permit)).await }
        });
        let registration_held = registration_pause.reached().await;
        let pid = rendition.last_child_pid.load(Relaxed);
        assert!(pid > 0, "actual FFmpeg was spawned before registration");
        rendition.closed.store(true, Release);
        if cancel_waiter {
            caller.abort();
        }
        assert_eq!(encoding.admissions.software_in_use(), used);
        registration_held.release();
        let failed_held = failed_wait.reached().await;
        assert_eq!(
            encoding.admissions.software_in_use(),
            used,
            "actual wait still unresolved"
        );
        failed_held.release();
        let retry_held = retry_wait.reached().await;
        assert_eq!(
            encoding.admissions.software_in_use(),
            used,
            "wait error retains actual permit"
        );
        retry_held.release();
        tokio::time::timeout(Duration::from_secs(5), async {
            while encoding.admissions.software_in_use() != 0 {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("confirmed owned cleanup");
        if cancel_waiter {
            assert!(caller.await.expect_err("cancelled caller").is_cancelled());
        } else {
            caller.await.expect("closed registration caller settles");
        }
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t, 0) }, -1);
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ESRCH)
        );
    }
}

/// Actual recipe/build preparation creates no reader demand or producer.
#[cfg(unix)]
#[tokio::test]
async fn source_deferred_rendition_preparation_creates_no_visible_demand_or_child() {
    let base = crate::test_tempdir().expect("deferred fixture");
    let (file, encoding) = encoded_fixture(base.path()).await;
    let serve = bare_serve(&base.path().join("renditions"));
    let req = request("deferred-viewer", 0.0);
    // This physical preparation fixture has no Source principal or binding.
    // Resolve the ordinary deferred recipe; never invent Source admission.
    let mut prepared = VodRecipeRequest::from(&req);
    prepared.encoding = Some(Arc::clone(&encoding));
    let (attachment, _) = serve
        .resolve_rendition(&mut prepared, &file, &settings(), None, None)
        .await
        .expect("actual prepared rendition");
    let rendition = Arc::clone(&attachment.rendition);
    assert!(serve.shared.sessions.lock().await.is_empty());
    assert!(rendition.readers.lock().await.is_empty());
    assert_eq!(rendition.last_child_pid.load(Relaxed), 0);
    assert_eq!(encoding.admissions.software_in_use(), 0);
    assert!(matches!(
        rendition.slot.belief().await,
        Producer::Absent { .. }
    ));
    drop(attachment);
    assert!(serve.shared.sessions.lock().await.is_empty());
    assert_eq!(rendition.last_child_pid.load(Relaxed), 0);
}

/// Source-only build/spawn enforce actual scanner facts and no-follow opening.
/// The literal cache namespace exercises the physical seam, not Source admission.
#[cfg(unix)]
#[tokio::test]
async fn source_physical_build_and_spawn_refuse_identity_drift_and_symlinks() {
    let base = crate::test_tempdir().expect("physical Source fixture");
    let (file, encoding) = encoded_fixture(base.path()).await;
    let serve = bare_serve(&base.path().join("renditions"));
    let plan = encoding.grid.plan(96_000, 428_000);
    let recipe = |file: MediaFile| Recipe {
        audio_delivery: None,
        measured_candidate: None,
        retained_logical: None,
        file,
        audio_index: None,
        aac: true,
        video: CopyVideoOptions::new(false, false),
        source_object_version: Some(encoding.source_object_version.clone()),
        cluster_cache_key: None,
        encoding: Some(Arc::clone(&encoding)),
    };
    let mut wrong = file.clone();
    wrong.size += 1;
    assert!(serve
        .shared
        .build_rendition(
            "source-wrong-size",
            None,
            recipe(wrong),
            plan.clone(),
            &settings(),
        )
        .await
        .is_err());
    let mut wrong = file.clone();
    wrong.mtime += 1;
    assert!(serve
        .shared
        .build_rendition(
            "source-wrong-mtime",
            None,
            recipe(wrong),
            plan.clone(),
            &settings(),
        )
        .await
        .is_err());
    let link = base.path().join("media-link.mkv");
    std::os::unix::fs::symlink(&file.path, &link).expect("actual symlink");
    let mut linked = file.clone();
    linked.path = link;
    assert!(serve
        .shared
        .build_rendition(
            "source-symlink",
            None,
            recipe(linked),
            plan.clone(),
            &settings(),
        )
        .await
        .is_err());
    let rendition = serve
        .shared
        .build_rendition(
            "source-physical-drift",
            None,
            recipe(file.clone()),
            plan,
            &settings(),
        )
        .await
        .expect("actual physical Source build");
    let permit = encoding.try_permit().await.expect("actual preadmission");
    assert!(encoding.admissions.software_in_use() > 0);
    use std::io::Write;
    std::fs::OpenOptions::new()
        .append(true)
        .open(&file.path)
        .expect("actual file mutation")
        .write_all(b"changed")
        .expect("drift");
    spawn_generation(&serve.shared, &rendition, 0, Some(permit)).await;
    assert_eq!(
        rendition.last_child_pid.load(Relaxed),
        0,
        "no media child on drift"
    );
    assert_eq!(
        encoding.admissions.software_in_use(),
        0,
        "never-spawned permit released"
    );
    assert!(rendition.failure().is_some());
}

/// A real produced cache must not launch an unadmitted Source head recovery.
#[cfg(unix)]
#[tokio::test]
async fn source_missing_cached_init_refuses_before_head_regeneration() {
    let base = crate::test_tempdir().expect("Source head fixture");
    let (file, encoding) = encoded_fixture(base.path()).await;
    let serve = bare_serve(&base.path().join("renditions"));
    let recipe = Recipe {
        audio_delivery: None,
        measured_candidate: None,
        retained_logical: None,
        file,
        audio_index: None,
        aac: true,
        video: CopyVideoOptions::new(false, false),
        source_object_version: Some(encoding.source_object_version.clone()),
        cluster_cache_key: None,
        encoding: Some(Arc::clone(&encoding)),
    };
    let plan = encoding.grid.plan(96_000, 428_000);
    let rendition = serve
        .shared
        .build_rendition(
            "source-head-refusal",
            None,
            recipe.clone(),
            plan.clone(),
            &settings(),
        )
        .await
        .expect("fresh real Source cache");
    let permit = encoding
        .try_permit()
        .await
        .expect("actual physical admission");
    spawn_generation(&serve.shared, &rendition, 0, Some(permit)).await;
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if rendition.dir.has_init().await
                && rendition.manifest.lock().await.materialized_count() > 0
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("actual cache publication");
    rendition
        .slot
        .perform(
            Step::Terminate {
                why: Termination::Idle,
            },
            || {},
        )
        .await
        .expect("actual child and writer settlement");
    assert_eq!(encoding.admissions.software_in_use(), 0);
    assert!(matches!(
        rendition.slot.belief().await,
        Producer::Absent { .. }
    ));
    std::fs::remove_file(rendition.dir.path().join("init.mp4")).expect("remove actual init");
    let refusal = match serve
        .shared
        .build_rendition("source-head-refusal", None, recipe, plan, &settings())
        .await
    {
        Ok(_) => panic!("unadmitted Source head recovery"),
        Err(error) => error,
    };
    assert!(refusal.contains("vod_source_head_pending"), "{refusal}");
    assert!(
        !rendition.dir.has_init().await,
        "head must not be regenerated"
    );
    assert_eq!(encoding.admissions.software_in_use(), 0);
}

/// Real Source copy argv and actual software permit/job/registered barrier.
/// This process fixture does not mint a Shared request or viewer actor.
#[cfg(unix)]
#[tokio::test]
async fn source_copy_actual_cpu_admission_and_bounded_argv_retain_until_reap() {
    use crate::vodencode::{EncodePermit, SourceCopyPermitRead, SOURCE_COPY_CPU_THREADS};
    use plurx_core::store::SettingsStore;
    use tokio::io::AsyncReadExt;
    let base = crate::test_tempdir().expect("Source copy fixture");
    let (file, _) = encoded_fixture(base.path()).await;
    let store = SqliteStore::open_in_memory().expect("policy store");
    let admissions = crate::admission::Admissions::new();
    store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "3")
        .await
        .expect("policy");
    assert!(matches!(
        EncodePermit::try_source_copy(&admissions, &store).await,
        SourceCopyPermitRead::Capacity
    ));
    assert_eq!(admissions.software_in_use(), 0);
    store
        .put_setting(plurx_core::store::keys::SW_POOL_THREADS, "4")
        .await
        .expect("policy");
    let permit = match EncodePermit::try_source_copy(&admissions, &store).await {
        SourceCopyPermitRead::Admitted(permit) => permit,
        _ => panic!("actual Source copy admission"),
    };
    assert_eq!(admissions.software_in_use(), SOURCE_COPY_CPU_THREADS);
    assert!(matches!(
        EncodePermit::try_source_copy(&admissions, &store).await,
        SourceCopyPermitRead::Capacity
    ));
    let recipe = Recipe {
        audio_delivery: None,
        measured_candidate: None,
        retained_logical: None,
        file,
        audio_index: None,
        aac: true,
        video: CopyVideoOptions::new(false, false),
        source_object_version: None,
        cluster_cache_key: None,
        encoding: None,
    };
    let ordinary = recipe_pipe_args(&recipe, 0.0, false);
    let mut args = ordinary.clone();
    bound_source_copy_threads(&mut args);
    for flag in ["-filter_threads", "-filter_complex_threads", "-threads:a"] {
        let index = args
            .iter()
            .position(|arg| arg == flag)
            .expect("explicit Source bound");
        assert_eq!(args[index + 1], "1");
        assert!(
            !ordinary.iter().any(|arg| arg == flag),
            "Local copy remains unchanged"
        );
    }
    for index in args
        .iter()
        .enumerate()
        .filter_map(|(i, arg)| (arg == "-i").then_some(i))
    {
        assert_eq!(&args[index - 2..index], &["-threads", "1"]);
    }
    let output = args.len() - 1;
    args.splice(output..output, ["-t".to_owned(), "0.25".to_owned()]);
    let mut command = tokio::process::Command::new(ffmpeg_bin());
    command
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped());
    let (mut child, job) = crate::process_control::spawn_job_owned(
        &mut command,
        crate::process_control::ChildWork::realtime("bounded Source copy fixture"),
    )
    .expect("actual copy child");
    let mut stdout = child.stdout.take().expect("copy output");
    let mut stderr = child.stderr.take().expect("copy diagnostic");
    let slot = crate::prodrun::ProducerSlot::new();
    let (registered, writers) = slot
        .attach_registered_job_owned(child, job, 0, Some(Box::new(permit)))
        .await;
    let output_reader = tokio::spawn(async move {
        let mut bytes = Vec::new();
        (&mut stdout)
            .take(1 << 20)
            .read_to_end(&mut bytes)
            .await
            .expect("actual copy bytes");
        bytes
    });
    let diagnostics = tokio::spawn(async move {
        let mut bytes = Vec::new();
        (&mut stderr)
            .take(1 << 20)
            .read_to_end(&mut bytes)
            .await
            .expect("actual diagnostics");
        bytes
    });
    let bytes = tokio::time::timeout(Duration::from_secs(5), output_reader)
        .await
        .expect("bounded copy completes")
        .expect("copy reader");
    let errors = diagnostics.await.expect("joined diagnostics");
    assert!(errors.is_empty(), "{}", String::from_utf8_lossy(&errors));
    assert!(bytes.windows(4).any(|part| part == b"moov"));
    assert!(bytes.windows(4).any(|part| part == b"mdat"));
    assert_eq!(admissions.software_in_use(), SOURCE_COPY_CPU_THREADS);
    assert!(registered.confirmed_reap().is_none());
    writers.settled();
    slot.request_registered_retirement(&registered)
        .await
        .expect("owned retirement");
    slot.wait_registered_retirement(&registered)
        .await
        .expect("actual successful wait+writers");
    assert_eq!(admissions.software_in_use(), 0);
}
