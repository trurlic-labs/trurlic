import unittest


def test_in_a_code_file():
    def inner():
        return 1

    return inner


class TestAudit:
    def compare(self):
        return 0


class AuditCase(unittest.TestCase):
    def setUp(self):
        self.value = 1


class TypedCase(unittest.TestCase[int]):
    def check(self):
        return 3


def tested():
    return 2
