import unittest

from solution import max_profit


class TestMaxProfit(unittest.TestCase):
    def test_example_1(self):
        self.assertEqual(max_profit([7, 1, 5, 3, 6, 4]), 5)

    def test_example_2(self):
        self.assertEqual(max_profit([7, 6, 4, 3, 1]), 0)

    def test_single(self):
        self.assertEqual(max_profit([1]), 0)

    def test_two(self):
        self.assertEqual(max_profit([1, 2]), 1)


if __name__ == "__main__":
    unittest.main()
