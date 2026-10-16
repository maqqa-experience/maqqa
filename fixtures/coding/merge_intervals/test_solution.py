import unittest

from solution import merge


class TestMergeIntervals(unittest.TestCase):
    def test_example_1(self):
        self.assertEqual(merge([[1, 3], [2, 6], [8, 10], [15, 18]]), [[1, 6], [8, 10], [15, 18]])

    def test_example_2(self):
        self.assertEqual(merge([[1, 4], [4, 5]]), [[1, 5]])

    def test_unsorted(self):
        self.assertEqual(merge([[4, 7], [1, 4]]), [[1, 7]])

    def test_single(self):
        self.assertEqual(merge([[1, 4]]), [[1, 4]])


if __name__ == "__main__":
    unittest.main()
