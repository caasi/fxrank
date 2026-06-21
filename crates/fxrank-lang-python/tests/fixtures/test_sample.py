def test_one():        # test_* function → skipped by default
    assert True

class TestThing:       # Test* class → its methods skipped
    def test_method(self):
        assert 1 == 1

import unittest
class MyCase(unittest.TestCase):
    def test_case(self):
        self.assertTrue(True)
