from __future__ import annotations

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


if __name__ == "__main__":
    unittest.main()
