use std::io::SeekFrom;

use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

#[tokio::test]
async fn anonymous_memory_file_round_trips_after_sealing() {
    let expected = b"authenticated snapshot";
    let mut file = plurx_core::fs_secure::anonymous_memory_file().expect("anonymous memory file");

    file.write_all(expected).await.expect("write snapshot");
    plurx_core::fs_secure::seal_anonymous_memory_file(&file).expect("seal snapshot");
    file.seek(SeekFrom::Start(0))
        .await
        .expect("rewind snapshot");

    let mut actual = Vec::new();
    file.read_to_end(&mut actual).await.expect("read snapshot");
    assert_eq!(actual, expected);
}
