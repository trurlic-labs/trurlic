use ids::numbered_id;

macro_rules! book_id {
    ($(#[$doc:meta])* catalog $name:ident) => {
        $(#[$doc])*
        pub struct $name(u32);
        impl $name {
            pub fn new(id: u32) -> Self { Self(id) }
            pub const fn raw(self) -> u32 { self.0 }
        }
    };
    ($name:ident) => {
        pub struct $name(u32);
    };
    (@inner $name:ident) => {};
}

book_id! {
    /// A catalog's identifier.
    catalog CatalogId
}
book_id!(Isbn);
numbered_id!(ShelfNumber(u8), noun = "shelf");

pub struct Catalog;
pub struct Shelf;
pub struct Imported;

pub fn call_only() {
    book_id!(@inner NotAType);
}
