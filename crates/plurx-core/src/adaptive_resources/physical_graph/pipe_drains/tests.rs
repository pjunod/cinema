//! Authored, not executed before complete-candidate independent review.
use super::*;
use tokio::io::AsyncWriteExt;

#[tokio::test]
async fn audited_discard_reads_beyond_one_buffer_and_closes_at_eof() {
    let (pipe, mut producer) = tokio::io::duplex(64);
    let task = tokio::spawn(discard(Some(pipe)));
    producer
        .write_all(&vec![1; BUFFER_BYTES * 3])
        .await
        .expect("bounded reader keeps draining");
    producer.shutdown().await.expect("EOF");
    task.await.expect("actual task join");
    assert!(producer.write_all(b"late").await.is_err());
}

#[tokio::test]
async fn audited_discard_abort_is_not_join_but_join_drops_original_pipe() {
    let (pipe, mut producer) = tokio::io::duplex(64);
    let task = tokio::spawn(discard(Some(pipe)));
    task.abort();
    // Awaiting the actual owned task, not observing an abort request or sender.
    assert!(task.await.expect_err("task was cancelled").is_cancelled());
    assert!(producer.write_all(b"no remaining receiver").await.is_err());
}

#[tokio::test]
async fn audited_discard_concurrent_streams_do_not_wait_for_other_stream_eof() {
    let (stdout, mut first) = tokio::io::duplex(1);
    let (stderr, mut second) = tokio::io::duplex(1);
    let task = tokio::spawn(async move {
        tokio::join!(discard(Some(stdout)), discard(Some(stderr)));
    });
    // Sequentially awaiting stdout EOF before stderr would deadlock this write.
    second
        .write_all(&vec![2; BUFFER_BYTES * 2])
        .await
        .expect("stderr independently drained");
    second.shutdown().await.expect("stderr EOF");
    first
        .write_all(b"stdout")
        .await
        .expect("stdout still drained");
    first.shutdown().await.expect("stdout EOF");
    task.await.expect("both owned streams dropped");
}
