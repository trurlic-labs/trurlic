/* A shelf of books in C: the source, read by the C grammar. */
#include <stdio.h>
#include "shelf.h"

#define SWAP(a, b) do { \
    book t = (a); /* the first */ \
    (a) = (b); (b) = t; \
  } while (0)

unsigned shelf_opened = 0;
static int verbose;
static int sort_books(shelf *s);

SHELF_API shelf *shelf_open(const char *path)
{
  struct local { int unused; };
  int pages = 0;
  (void)path;
  shelf_opened++;
  return 0;
}

SHELF_API void shelf_close(shelf *s) { (void)s; }

static int sort_books(shelf *s)
{
  return s->count > 1;
}

#if defined(_WIN32)
static long weigh(shelf *s) { return (long)s->count; }
#else
static long weigh(shelf *s) { return (long)s->count * 2; }
#endif

int (*pick(int which))(const book *, const book *) { (void)which; return 0; }

int legacy(a, b)
  int a;
  char *b;
{
  return a + (b != 0);
}

SHELF_API int shelf_log(shelf *s, const char *format, ...)
{
  (void)s;
  return printf("%s", format);
}

/* A macro no file of the tree defines stays: tree-sitter's recovery
 * still reads this function. */
SYSTEM_EXPORT int shelf_outside(void) { return 0; }
