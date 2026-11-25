//! A case for each rule a review of the index found broken: a negative
//! literal, a repetition's separator, arms at the bound of words, the
//! impls a type-defining macro gives its type, a spelled path, a comment
//! between an attribute and its item, a name re-exported twice, an
//! absolute `#[path]` and a module declared in a function.

pub fn sign(x: i8) -> bool {
    match x {
        -1 => true,
        _ => false,
    }
}

macro_rules! listed {
    ($($x:ident),*) => { $($x);* };
}

macro_rules! worded {
    (a b c d e f g h $name:ident) => { pub struct $name; };
    (a b c d e f g h i $name:ident) => { pub struct $name; };
}

worded!(a b c d e f g h Eight);
worded!(a b c d e f g h i Nine);

macro_rules! owned {
    ($name:ident) => {
        pub struct $name;
        impl $name {
            pub fn get(&self) {}
        }
        impl std::fmt::Display for $name {
            fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                Ok(())
            }
        }
        impl From<$name> for String {
            fn from(x: $name) -> String {
                String::new()
            }
        }
    };
}

owned!(Owned);

record! { enum other::Named { X } }

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

#[cfg(unix)]
owned!(Twin);
#[cfg(not(unix))]
owned!(Twin);

macro_rules! declare {
    ([$($doc:tt)*] $mode:ident $name:ident) => {
        pub struct $name;
        impl $name {
            pub fn declared(&self) {}
        }
    };
}

macro_rules! relay {
    ([$($doc:tt)*] $name:ident) => {
        declare!([] plain $name);
    };
}

macro_rules! entity {
    (record $name:ident at $place:literal) => {
        declare!([$place] plain $name);
    };
    ($name:ident relayed) => {
        relay!([] $name);
    };
}

entity!(record Recorded at "here");
entity!(Relayed relayed);
