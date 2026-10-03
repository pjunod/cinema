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
    assert!(!independent.is_finished(), "association cannot settle before actual process/writers");
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
    assert!(independent.await.expect("independent generation association").matches(&generation));
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
        Box::pin(async move { self.0.hold().await; })
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
        let mut rendition = serve.shared.build_rendition(
            "owned-registration", None, Recipe {
                file, audio_index: None, aac: true,
                video: CopyVideoOptions::new(false,false),
                source_object_version: Some(encoding.source_object_version.clone()),
                cluster_cache_key: None, encoding: Some(Arc::clone(&encoding)),
            }, encoding.grid.plan(96_000,428_000), &settings(),
        ).await.expect("actual encoded rendition");
        let registration_pause = crate::seam_hooks::AsyncPause::new("owned registration after actual spawn");
        Arc::get_mut(&mut rendition).expect("unpublished rendition").hooks = Box::new(SourceRegistrationPause(Arc::clone(&registration_pause)));
        let failed_wait = crate::seam_hooks::AsyncPause::new("owned postspawn wait failure");
        let retry_wait = crate::seam_hooks::AsyncPause::new("owned postspawn wait retry");
        rendition.slot.set_reap_hooks(Arc::new(SourceLifetimeWait {
            pause: Arc::clone(&failed_wait), retry: Arc::clone(&retry_wait), fail_once: AtomicBool::new(true),
        })).await;
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
        if cancel_waiter { caller.abort(); }
        assert_eq!(encoding.admissions.software_in_use(),used);
        registration_held.release();
        let failed_held = failed_wait.reached().await;
        assert_eq!(encoding.admissions.software_in_use(),used,"actual wait still unresolved");
        failed_held.release();
        let retry_held = retry_wait.reached().await;
        assert_eq!(encoding.admissions.software_in_use(),used,"wait error retains actual permit");
        retry_held.release();
        tokio::time::timeout(Duration::from_secs(5), async {
            while encoding.admissions.software_in_use() != 0 { tokio::time::sleep(Duration::from_millis(10)).await; }
        }).await.expect("confirmed owned cleanup");
        if cancel_waiter { assert!(caller.await.expect_err("cancelled caller").is_cancelled()); }
        else { caller.await.expect("closed registration caller settles"); }
        assert_eq!(unsafe { libc::kill(pid as libc::pid_t,0) },-1);
        assert_eq!(std::io::Error::last_os_error().raw_os_error(),Some(libc::ESRCH));
    }
}
