// A shelf of books in C++: the source.
#include "shelf.hpp"

// A namespace a macro opens: its definitions lack its segments.
SHELF_BEGIN_NAMESPACE

int stocked() { return 1; }

SHELF_END_NAMESPACE

namespace shelf::model {

int Book::count = 0;

Book::Book() = default;
Book::Book(std::string title) : title_(std::move(title)) {}
Book::~Book() {}

bool Book::operator == (const Book& other) const { return pages_ == other.pages_; }

Book::operator int() const { return pages_; }

int Book::operator()(int page) const { return page < pages_ ? page : -1; }

void Book::mark(int page) {
  struct Marker { int at; };
  Marker m{page};
  (void)m;
}

bool operator<(const Book& a, const Book& b) { return a.pages() < b.pages(); }

template <typename T> T Box<T>::get() const { return value_; }

Book Box<Book>::get() const { return Book(); }

template <> Book heavier<Book>(Book a, Book b) { return a.pages() < b.pages() ? b : a; }

int weigh(const Book& b) { return b.pages(); }
int weigh(const Box<Book>& b) { return b.get().pages(); }

#if defined(SHELF_FAST)
static int tidy(Shelf& s) { return static_cast<int>(s.size()); }
#else
static int tidy(Shelf& s) { s.clear(); return 0; }
#endif

}  // namespace shelf::model
