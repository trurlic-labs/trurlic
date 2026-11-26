/* A shelf of books in C: the header, read by the C++ grammar as every
 * `.h` is. One construct per line or block. */
#ifndef SHELF_H
#define SHELF_H

/* Attribute macros the tree defines: blanked before the parse. */
#if defined(_WIN32)
#define SHELF_API __declspec(dllexport)
#else
#define SHELF_API __attribute__((visibility("default")))
#endif
#define SHELF_PRINTF(f, a) __attribute__((format(printf, f, a)))

/* Specifiers the tree defines that hold `static`, as every build but the
 * tests' expands them: what each marks is private, however short its
 * name. */
#ifdef SHELF_TESTING
#define SHELF_STATIC
#define SI
#else
#define SHELF_STATIC static
#define SI static inline
#endif

/* A macro that writes a member: the member is no definition. */
#define SHELF_FLAG(name) unsigned name : 1

#ifdef __cplusplus
extern "C" {
#endif

#define SHELF_CAPACITY 40
#define SHELF_HALF(n) ((n) / 2)

typedef struct shelf shelf;
typedef struct { int pages; } book;
typedef struct named_book { int pages; } named_book_t;
typedef int (*shelf_visit)(const book *b, void *data);

enum shelf_state { SHELF_OPEN, SHELF_CLOSED = 2, SHELF_DONE };

struct shelf {
  book *books;
  unsigned count;
  int (*compare)(const book *, const book *);
  struct { int row, column; } place;
  union { long id; char tag[8]; };
  SHELF_FLAG(full);
};

SHELF_API shelf *shelf_open(const char *path);
SHELF_API void shelf_close(shelf *s);
SHELF_API int shelf_log(shelf *s, const char *format, ...) SHELF_PRINTF(2, 3);
SHELF_API void (*shelf_on_close(shelf *s, void (*hook)(shelf *)))(shelf *);
int (shelf_max)(int a, int b);
int (*(*shelf_pick_pick(int which))(int))(int);
void (shelf_reset) (shelf_cursor *c);
SHELF_API const char *(shelf_title)(const shelf *s);
extern unsigned shelf_opened;

#ifdef __cplusplus
}
#endif

#endif /* SHELF_H */
