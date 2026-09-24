from __future__ import annotations

from pathlib import Path
import textwrap
import unittest

from validation.test_markers import defines_test


RUST = textwrap.dedent(
    """\
    #[test]
    fn a_plain_test() {
        assert!(true);
    }

    fn a_helper_below_a_test() -> u8 {
        1
    }

    #[tokio::test(
        flavor = "multi_thread",
        worker_threads = 2
    )]
    async fn a_wrapped_multi_thread_test() {
        a_helper_below_a_test();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_current_thread_test() {}

        #[test]
        #[cfg(unix)]
        /// Doc comments sit between the attribute and the declaration.
        fn a_documented_test() {}
    """
)


class DefinesTestCase(unittest.TestCase):
    """What a `Regression-Test:` name has to point at to count as a test."""

    def test_a_tokio_test_with_arguments_is_a_test(self):
        # This repository spells hundreds of async tests
        # `#[tokio::test(flavor = ...)]`; a marker set that only knew the bare
        # `#[tokio::test]` refused every one of them.
        self.assertTrue(defines_test(RUST, "a_current_thread_test"))
        self.assertTrue(defines_test(RUST, "a_wrapped_multi_thread_test"))

    def test_a_helper_cannot_borrow_the_attribute_of_the_test_above_it(self):
        # The marker walk stops at the end of the previous item, so a plain
        # function declared a few lines under a test is not a test.
        self.assertFalse(defines_test(RUST, "a_helper_below_a_test"))

    def test_the_ordinary_forms_are_still_tests(self):
        self.assertTrue(defines_test(RUST, "a_plain_test"))
        self.assertTrue(defines_test(RUST, "a_documented_test"))
        self.assertTrue(defines_test('test("the node title", () => {})\n', "the node title"))
        self.assertTrue(defines_test("    def test_something(self):\n", "test_something"))
        self.assertTrue(defines_test("    @Test\n    fun decodesTheFrame() {\n", "decodesTheFrame"))
        self.assertFalse(defines_test(RUST, "no_such_test"))

    def test_a_test_prefixed_helper_needs_its_marker_where_the_language_does(self):
        # Rust, Kotlin and Java run only annotated tests; this repository has
        # dozens of Rust `fn test_…` fixture builders inside `mod tests`
        # (`secrets.rs::test_key` among them). Accepting the name alone let a
        # `Regression-Test:` line cite a helper as a test.
        rust = (
            "#[cfg(test)]\nmod tests {\n    #[test]\n    fn a_test() {}\n\n"
            "    fn test_key() -> [u8; 32] {\n        [7; 32]\n    }\n}\n"
        )
        self.assertFalse(defines_test(rust, "test_key"))
        self.assertTrue(defines_test(rust, "a_test"))
        self.assertFalse(defines_test("    fun testFixture(): Int = 1\n", "testFixture"))
        self.assertFalse(defines_test("    public void testHelper() {\n", "testHelper"))
        # XCTest runs parameterless instance methods only.
        self.assertFalse(
            defines_test("    static func testing(\n        _ x: Int\n    ) {}\n", "testing")
        )
        self.assertFalse(
            defines_test("    func testHelper(_ value: Int) {}\n", "testHelper")
        )
        # Where the name is the marker, it still is.
        self.assertTrue(defines_test("    func testTheWatchdogStops() {\n", "testTheWatchdogStops"))
        self.assertTrue(
            defines_test("    @MainActor func testTheDockHolds() async {\n", "testTheDockHolds")
        )
        self.assertTrue(defines_test("    def test_the_cut(self):\n", "test_the_cut"))

    def test_an_annotation_on_the_declaration_line_marks_it(self):
        # `@Test fun name()` on one line is how 32 Android tests here are
        # written, and all 32 were refused.
        kotlin = (
            "class T {\n"
            "    @Test fun typedConflictKeepsItsCode() {\n        assertEquals(1, 1)\n    }\n\n"
            "    @Test fun oneLiner() = runTest { }\n"
            "    fun helperBelowAOneLiner() = 1\n"
            "}\n"
        )
        self.assertTrue(defines_test(kotlin, "typedConflictKeepsItsCode"))
        self.assertTrue(defines_test(kotlin, "oneLiner"))
        # A one-line annotated declaration is its own item; the helper below
        # it does not borrow its `@Test`.
        self.assertFalse(defines_test(kotlin, "helperBelowAOneLiner"))
        self.assertTrue(defines_test("#[test] fn a_one_line_rust_test() {}\n", "a_one_line_rust_test"))
        self.assertTrue(
            defines_test(
                '#[tokio::test(flavor = "current_thread")] async fn inline_async() {}\n',
                "inline_async",
            )
        )
        self.assertFalse(
            defines_test("#[inline] fn test_not_a_test() {}\n", "test_not_a_test")
        )

    def test_the_reviewed_repository_cases(self):
        """The two real names PR #485's review cited, on this tree."""

        root = Path(__file__).resolve().parents[2]
        secrets = (root / "crates/plurx-core/src/secrets.rs").read_text(encoding="utf-8")
        self.assertFalse(defines_test(secrets, "test_key"))
        kotlin = (
            root
            / "clients/android/app/src/test/java/tv/plurx/app/librarychannels"
            / "LibraryChannelStartFailureTest.kt"
        ).read_text(encoding="utf-8")
        self.assertTrue(defines_test(kotlin, "typedConflictKeepsItsRecoveryCodeAndMessage"))



if __name__ == "__main__":
    unittest.main()
