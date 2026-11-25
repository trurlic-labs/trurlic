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


def tested():
    return 2
