"""A shelf of books: one per construct the Python resolver reads."""

from __future__ import annotations

import os
from typing import TYPE_CHECKING, overload

CAPACITY = 40
LABEL: str = "main"
WIDTH = DEPTH = 12
FIRST, (SECOND, [THIRD, *REST]) = 1, (2, [3, 4, 5])
_REGISTRY: dict = {}
__all__ = ["Shelf", "open_shelf"]
type Shelves = list[Shelf]
type Pair[T] = tuple[T, T]

if TYPE_CHECKING:
    Path = os.PathLike
else:
    Path = str

try:
    import fast_json as json
except ImportError:
    def loads(text):
        return text


def open_shelf(path: Path) -> Shelf:
    """Open the shelf at `path`."""
    found = Shelf(path)

    def check(shelf):
        return shelf.size >= 0

    check(found)
    return found


async def fetch(path):
    return await os.stat(path)


def _cached(key):
    return _REGISTRY.get(key)


@overload
def width(book: int) -> int: ...
@overload
def width(book: str) -> int: ...
def width(book):
    return len(f"{book: >4}{book = }")


def tupled():
    "a", width(1)


class Shelf:
    """Books in a row."""

    limit: int = CAPACITY
    label = "shelf"
    _hidden = 0

    def __init__(self, path):
        self.path = path
        count = 0
        self.count = count

    @property
    def size(self):
        return self.count

    @size.setter
    def size(self, value):
        self.count = value

    @staticmethod
    def blank():
        return Shelf("")

    def _tidy(self):
        def sort_key(book):
            return book

        return sort_key

    class Slot:
        index = 0

        def take(self):
            return self.index


@dataclass(frozen=True)
class Book:
    title: str
    pages: int = 0
