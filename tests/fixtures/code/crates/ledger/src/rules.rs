//! A case for each rule a review of the index found broken. They are a
//! negative literal, a repetition's separator, a rule's opening and a
//! group at their bounds, and the impls a type-defining macro gives its
//! type. They are a spelled path, a comment between an attribute and its
//! item, a name re-exported twice, an absolute `#[path]` and a module
//! declared in a function.

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
    (a b c d e f g h i j k l m n o $name:ident) => { pub struct $name; };
    (a b c d e f g h i j k l m n o p $name:ident) => { pub struct $name; };
}

worded!(a b c d e f g h i j k l m n o Sixteenth);
worded!(a b c d e f g h i j k l m n o p Seventeenth);

macro_rules! grouped {
    ([a b c d e f g x] $name:ident) => { pub struct $name; };
    ([a b c d e f g y] $name:ident) => { pub enum $name {} };
    ([a b c d e f g h x] $name:ident) => { pub struct $name; };
    ([a b c d e f g h y] $name:ident) => { pub enum $name {} };
}

grouped!([a b c d e f g y] EightTold);
grouped!([a b c d e f g h y] NineUntold);

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

pub enum Disc {
    A = {
        pub const K: isize = 1;
        K
    },
}

fn recovered() {
    let x = 1 @@ 2;
}

spelled! {
    default fn defaulted() {}
    enum Source as Alias { A }
}

macro_rules! paired {
    ($a:ident, $b:ident) => {
        pub struct $a;
        pub struct $b;
        impl $b {
            fn only_second() {}
        }
        impl Tr<$a> for $a {
            fn tr() {}
        }
    };
}

paired!(First, Second);

macro_rules! front {
    ($name:ident in $module:literal) => {
        declare!([] plain $name);
    };
    (compared $name:ident in $module:literal) => {
        $crate::declare!([] plain $name);
    };
}

front!(Plain in "m");
front!(compared Shown in "m");
front!(enum in "m");

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
