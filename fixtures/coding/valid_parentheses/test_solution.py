import unittest

from solution import is_valid


class TestValidParentheses(unittest.TestCase):
    def test_simple(self):
        self.assertTrue(is_valid("()"))

    def test_mixed(self):
        self.assertTrue(is_valid("()[]{}"))

    def test_wrong(self):
        self.assertFalse(is_valid("(]"))

    def test_nested(self):
        self.assertTrue(is_valid("([])"))

    def test_empty(self):
        self.assertTrue(is_valid(""))

    def test_unbalanced(self):
        self.assertFalse(is_valid("([)]"))


if __name__ == "__main__":
    unittest.main()
