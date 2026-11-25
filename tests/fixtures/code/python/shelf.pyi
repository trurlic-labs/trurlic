from typing import overload

CAPACITY: int

def open_shelf(path: str) -> Shelf: ...

class Shelf:
    limit: int
    def __init__(self, path: str) -> None: ...
    @property
    def size(self) -> int: ...
