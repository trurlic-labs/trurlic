// A shelf of books in C++: the header. One construct per line or block.
#pragma once
#include <string>
#include <vector>

#define SHELF_API __attribute__((visibility("default")))
#define SHELF_CONSTEXPR constexpr
#define SHELF_BEGIN_NAMESPACE namespace shelf { inline namespace v1 {
#define SHELF_END_NAMESPACE } }

namespace shelf::model {

enum class Format { kPaper, kDigital = 2 };

class SHELF_API Book {
 public:
  Book();
  explicit Book(std::string title);
  ~Book();
  bool operator==(const Book& other) const;
  Book& operator=(const Book&) = default;
  explicit operator bool() const { return !title_.empty(); }
  operator int() const;
  int operator()(int page) const;
  SHELF_CONSTEXPR int pages() const { return pages_; }
  static int count;
  static int total() { return count; }
  virtual void read() = 0;
  friend bool operator<(const Book& a, const Book& b);
  friend void swap(Book& a, Book& b) { std::swap(a.pages_, b.pages_); }
  using title_type = std::string;
  struct Page { int number; };

 protected:
  void mark(int page);

 private:
  std::string title_;
  int pages_ = 0;
};

class Ledger {
  int entries;

 public:
  int size() const { return entries; }
};

struct Spine {
  int width;
 private:
  int glue;
};

template <typename T> class Box {
 public:
  T get() const;
 private:
  T value_;
};

template <> class Box<Book> {
 public:
  Book get() const;
};

template <typename T> T heavier(T a, T b) { return a < b ? b : a; }
template <> Book heavier<Book>(Book a, Book b);

int weigh(const Book& b);
int weigh(const Box<Book>& b);

using Shelf = std::vector<Book>;
typedef int (*visit)(const Book&);

inline auto by_pages = [](const Book& a, const Book& b) { return a.pages() < b.pages(); };

namespace edition { inline namespace v2 { int number(); } }

}  // namespace shelf::model

namespace {
int hidden_count = 0;
}

static int shelved = 0;

// A macro no file of the tree defines stays, and the class it marks is
// misread: a limit.
class SYSTEM_EXPORT Catalog { int size; };
