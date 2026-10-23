include!("tests/chunk_01.rs");
include!("tests/chunk_02.rs");
include!("tests/chunk_03.rs");
include!("tests/source_lifetime.rs");
include!("tests/retained_durable.rs");
include!("tests/copy_preparation.rs");
include!("tests/encoded_preparation.rs");

include!("tests/preparation_storage.rs");
#[cfg(all(target_os = "linux", feature = "cluster-integration-tests"))]
include!("tests/finite_software.rs");
#[cfg(all(target_os = "linux", feature = "cluster-integration-tests"))]
include!("tests/finite_software_restart.rs");
