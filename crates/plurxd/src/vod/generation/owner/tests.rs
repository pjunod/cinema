//! Authored with the production owner; execute only after final candidate review.
use super::*;

async fn fixture() -> (
    tempfile::TempDir,
    Arc<Shared>,
    Arc<Rendition>,
    Arc<GenerationOutput>,
) {
    let root = crate::test_tempdir().expect("VOD terminal fixture");
    let serve = super::super::super::tests::bare_serve(root.path());
    let rendition = super::super::super::tests::synthetic_rendition(root.path()).await;
    (
        root,
        Arc::clone(&serve.shared),
        rendition,
        GenerationOutput::new(),
    )
}

#[tokio::test]
async fn output_notification_carries_no_sink_and_slot_retains_terminal_custody() {
    let (_root, shared, rendition, output) = fixture().await;
    let weak = Arc::downgrade(&output);
    let resources = GenerationResources {
        _admission: None,
        _output: Arc::clone(&output),
    };
    let (notify, observer) = tokio::sync::oneshot::channel::<()>();
    drop(observer);
    output.state.lock().await.terminal = Some(GenerationTerminal {
        outcome: None,
        completed: Some((
            RenditionSink {
                shared,
                rendition,
                epoch: 0,
                retirement: tokio_util::sync::CancellationToken::new(),
            },
            true,
        )),
        diagnostic: "owned terminal".into(),
    });
    assert!(notify.send(()).is_err());
    drop(output);
    let retained = weak.upgrade().expect("slot still owns terminal");
    assert!(retained
        .state
        .lock()
        .await
        .terminal
        .as_ref()
        .unwrap()
        .completed
        .is_some());
    drop(retained);
    drop(resources);
    assert!(
        weak.upgrade().is_none(),
        "no owner cycle after actual disposal"
    );
}

#[tokio::test]
async fn cancelled_terminal_observer_keeps_actual_sink_in_the_same_owner() {
    let (_root, shared, rendition, output) = fixture().await;
    output.state.lock().await.terminal = Some(GenerationTerminal {
        outcome: None,
        completed: Some((
            RenditionSink {
                shared: Arc::clone(&shared),
                rendition: Arc::clone(&rendition),
                epoch: rendition.gen_epoch.load(Relaxed),
                retirement: tokio_util::sync::CancellationToken::new(),
            },
            true,
        )),
        diagnostic: "retained through cancellation".into(),
    });
    let manifest = rendition.manifest.lock().await;
    {
        let finish = output.finish(&shared, &rendition, rendition.gen_epoch.load(Relaxed), 0);
        tokio::pin!(finish);
        tokio::select! {
            biased;
            _ = &mut finish => panic!("completion must wait for the manifest"),
            _ = tokio::task::yield_now() => {},
        }
        assert!(
            output.state.try_lock().is_err(),
            "observer borrows rather than takes output"
        );
    }
    let state = output.state.lock().await;
    let terminal = state
        .terminal
        .as_ref()
        .expect("cancelled metadata retains payload");
    let (sink, _) = terminal.completed.as_ref().expect("actual sink retained");
    assert!(Arc::ptr_eq(&sink.rendition, &rendition));
    assert_eq!(terminal.diagnostic, "retained through cancellation");
    drop(state);
    drop(manifest);
    output
        .finish(&shared, &rendition, rendition.gen_epoch.load(Relaxed), 0)
        .await;
    assert!(output.state.lock().await.terminal.is_none());
}

#[tokio::test]
async fn readiness_refusal_has_no_terminal_output_or_completion_receipt() {
    let (_root, shared, rendition, output) = fixture().await;
    assert!(output.state.lock().await.terminal.is_none());
    output
        .finish(&shared, &rendition, rendition.gen_epoch.load(Relaxed), 0)
        .await;
    assert!(output.state.lock().await.terminal.is_none());
    // This local output operation has no physical-close or refund API.
}
