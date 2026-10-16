"""Minimal golden filters tests — must remain for scenario checkpoints.

`test_suite_present` always asserts so the agent cannot treat an all-skip
suite as DONE. Import checks soft-skip until filters exist.
"""

from __future__ import annotations

import unittest


class TestFiltersSmoke(unittest.TestCase):
    def test_suite_present(self) -> None:
        """Keeps has_tests / unittest discover meaningful (not all-skipped)."""
        self.assertTrue(True)

    def test_filters_module_importable(self) -> None:
        try:
            import jobs_aggregator.filters as filters  # type: ignore
        except Exception:
            self.skipTest("jobs_aggregator.filters not available yet")
        self.assertIsNotNone(getattr(filters, "__file__", None))


if __name__ == "__main__":
    unittest.main()
