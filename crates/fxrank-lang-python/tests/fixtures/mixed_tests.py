# A regular file (not named test_*.py or *_test.py) that contains
# a mix of test and non-test units, for source-based skip testing.

def normal_function():
    return 42

def test_something():          # test_* function → skipped by source-based rule
    assert normal_function() == 42

class TestWidget:              # Test* class → its methods skipped
    def test_render(self):
        pass
    def helper(self):          # method of a Test* class → skipped (all its methods are)
        pass

import unittest
class MyCase(unittest.TestCase):    # unittest.TestCase subclass → methods skipped
    def test_case(self):
        self.assertTrue(True)
