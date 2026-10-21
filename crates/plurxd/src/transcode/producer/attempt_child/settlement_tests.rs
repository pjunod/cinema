use super::*;

#[tokio::test]
async fn cancelled_supervisor_join_keeps_the_actual_task_for_retry() {
    let (release, released) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        released.await.expect("release actual supervisor");
    });
    let mut owner = AttemptSupervisorJoin::Running(task);
    assert!(
        tokio::time::timeout(Duration::from_millis(1), owner.settle())
            .await
            .is_err()
    );
    assert!(matches!(&owner, AttemptSupervisorJoin::Running(task) if !task.is_finished()));
    release.send(()).expect("task was not cancelled");
    owner.settle().await.expect("join same actual task");
    owner.settle().await.expect("joined state is idempotent");
    assert!(matches!(owner, AttemptSupervisorJoin::Joined(Ok(()))));
}

#[tokio::test]
async fn failed_supervisor_join_never_turns_into_a_later_success() {
    let task = tokio::spawn(async { panic!("intentional supervisor failure") });
    let mut owner = AttemptSupervisorJoin::Running(task);
    let first = owner.settle().await.expect_err("panic is not settlement");
    let second = owner.settle().await.expect_err("retry retains failure");
    assert_eq!(first.kind(), second.kind());
    assert_eq!(first.to_string(), second.to_string());
    assert!(matches!(owner, AttemptSupervisorJoin::Joined(Err(_))));
}
