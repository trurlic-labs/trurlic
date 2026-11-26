macro_rules! book_id {
    ($(#[$doc:meta])* catalog $name:ident) => {
        $(#[$doc])*
        pub struct $name(u32);
        impl $name {
            pub fn new(id: u32) -> Self { Self(id) }
        }
    };
    (@inner $name:ident) => {};
}

book_id! {
    /// A catalog's identifier.
    catalog CatalogId
}

pub struct Catalog;
pub struct Shelf;
pub struct Imported;

pub fn call_only() {
    book_id!(@inner NotAType);
}
