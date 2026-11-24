//! Exported type-defining macros, which every crate of the workspace sees.

#[macro_export]
macro_rules! numbered_id {
    ($(#[$doc:meta])* $name:ident($repr:ty), noun = $noun:literal) => {
        $(#[$doc])*
        pub struct $name($repr);
        impl $name {
            pub fn get(self) -> $repr { self.0 }
        }
    };
}

macro_rules! crate_only_id {
    ($name:ident) => { pub struct $name(u8); };
}

crate_only_id! { Local }
