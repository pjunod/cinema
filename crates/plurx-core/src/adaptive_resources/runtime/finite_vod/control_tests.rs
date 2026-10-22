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
