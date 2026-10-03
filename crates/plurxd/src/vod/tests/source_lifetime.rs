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
