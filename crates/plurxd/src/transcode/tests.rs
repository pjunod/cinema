include!("tests/chunk_01.rs");
include!("tests/chunk_02.rs");
include!("tests/chunk_03.rs");
include!("tests/chunk_04.rs");
include!("tests/chunk_05.rs");
include!("tests/chunk_06.rs");
include!("tests/chunk_07.rs");
include!("tests/chunk_08.rs");
include!("tests/chunk_09.rs");

// TRANSCODE-DECOMPOSITION-PLAN §7 Q3: the syn pass over test-only seams.
#[path = "tests/seam_census.rs"]
mod seam_census;
