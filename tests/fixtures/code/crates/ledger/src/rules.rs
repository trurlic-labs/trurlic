//! A case for each rule a review of the index found broken. They are a
//! negative literal, a comment between an attribute and its item, a name
//! re-exported twice, an absolute `#[path]`, a module declared in a
//! function, items alike but for their `cfg`, an item in a discriminant,
//! a file mid-edit, a raw identifier, and an impl's arguments with a
//! comment and the trailing comma rustfmt adds when it breaks them.

pub fn sign(x: i8) -> bool {
    match x {
        -1 => true,
        _ => false,
    }
}

#[cfg(test)]
// A plain comment between the attribute and its module.
mod commented {
    fn helper() {}
}

pub use crate::books::{Catalog as Listed, Catalog as Listed};

#[cfg(test)]
#[path = "/abs.rs"]
mod absolute;

fn scoped() {
    #[cfg(test)]
    mod in_body;
}

#[cfg(unix)]
pub fn flush() {}

#[cfg(not(unix))]
pub fn flush() {}

pub enum Disc {
    A = {
        pub const K: isize = 1;
        K
    },
}

fn recovered() {
    let x = 1 @@ 2;
}

mod x {
    pub mod a {
        #[cfg(unix)]
        pub fn spread() {}
        #[cfg(not(unix))]
        pub fn spread() {}
    }
}

mod y {
    pub mod a {
        pub fn spread() {}
    }
}

pub struct r#Raw;

impl
    From<
        // why
        u8,
    > for r#Raw
{
    fn from(_: u8) -> Self {
        Self
    }
}
