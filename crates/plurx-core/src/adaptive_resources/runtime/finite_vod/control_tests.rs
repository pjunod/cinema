//! The stop intent must survive the launch/registration boundary.
use super::*;
#[tokio::test]
async fn stop_requested_before_registration_kills_the_original_late_child() {
    let control = FiniteControl::new();
    control.request().expect("stop before native launch");
    let mut command = tokio::process::Command::new("/bin/sleep");
    command.arg("60").kill_on_drop(true);
    let (mut child, job) = crate::process::spawn_job_custodied(
        &mut command,
        crate::process::ChildWork::realtime("finite stop fixture"),
    )
    .map_err(|failure| failure.error.to_string())
    .expect("actual retained fixture child");
    control
        .install(job.registered_control().expect("original registration"))
        .expect("install rechecks pending stop intent");
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
        .await
        .expect("original child must be signalled immediately")
        .expect("wait actual child");
    assert!(!status.success());
    drop(job);
    control
        .request()
        .expect("removed registration cannot select another PID");
}

#[tokio::test]
async fn stop_with_a_registered_root_kills_it_and_leaves_the_writer_to_finish() {
    let control = FiniteControl::new();
    let mut command = tokio::process::Command::new("/bin/sleep");
    command.arg("60").kill_on_drop(true);
    let (mut child, job) = crate::process::spawn_job_custodied(
        &mut command,
        crate::process::ChildWork::realtime("finite stop fixture"),
    )
    .map_err(|failure| failure.error.to_string())
    .expect("actual retained fixture child");
    control
        .install(job.registered_control().expect("original registration"))
        .expect("install the original root control");
    // The writer ends only when its input ends, as a real producer reader does.
    let (input_closed_tx, input_closed_rx) = tokio::sync::oneshot::channel::<()>();
    let writer = tokio::spawn(async move {
        let _ = input_closed_rx.await;
        "writer reached its own terminal"
    });
    control
        .install_task(writer.abort_handle())
        .expect("retain original task control");

    control.request().expect("stop the original producer");
    let status = tokio::time::timeout(std::time::Duration::from_secs(5), child.wait())
        .await
        .expect("the registered root is killed")
        .expect("wait actual child");
    assert!(!status.success());
    assert!(
        !writer.is_finished(),
        "stop must not abort a writer whose root was killed"
    );
    drop(input_closed_tx);
    let joined = tokio::time::timeout(std::time::Duration::from_secs(5), writer)
        .await
        .expect("writer finishes after its input ends")
        .expect("the writer was not aborted");
    assert_eq!(joined, "writer reached its own terminal");
    drop(job);
}
