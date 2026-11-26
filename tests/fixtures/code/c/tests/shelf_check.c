/* A check of the shelf: test code by its place, a `tests/` directory. */
#include "../shelf.h"

static int check_open(void) { return shelf_open("x") == 0; }

int main(void) { return check_open() ? 0 : 1; }
