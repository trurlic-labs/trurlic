// googletest's tests of the shelf: test code by the file's name, and each
// test by its macro.
#include "shelf.hpp"

namespace shelf::model {

class ShelfTest {
 protected:
  int opened_ = 0;
};

TEST(BookTest, StartsEmpty) {
  int helper = 0;
  (void)helper;
}

TEST_F(ShelfTest, Opens) {}

TEST_P(ShelfTest, OpensEach) {}

template <typename T> class BoxTest {};
TYPED_TEST(BoxTest, Holds) {}
TYPED_TEST_P(BoxTest, HoldsEach) {}

}  // namespace shelf::model
