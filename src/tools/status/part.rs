//! A section's items: before the budget cuts them ([`Listed`]), and
//! those that fit, as the report shows them ([`Part`]).

use crate::tools::spec::output::{Count, Items, Nested, Str};
use crate::tools::spec::{Output, Shown};

/// A section's items before the budget cuts them, out of `total`: every
/// item but a ranked section's, which holds only the best a result can.
pub(super) struct Listed<T> {
    pub(super) total: usize,
    pub(super) items: Vec<T>,
}

impl<T> Listed<T> {
    pub(super) fn all(items: impl IntoIterator<Item = T>) -> Self {
        let items: Vec<T> = items.into_iter().collect();
        Self {
            total: items.len(),
            items,
        }
    }

    /// The first `items` of `total`.
    pub(super) fn of(total: usize, items: impl IntoIterator<Item = T>) -> Self {
        Self {
            total,
            items: items.into_iter().collect(),
        }
    }

    /// The section with none of its items shown: what a report measures
    /// before it is filled.
    pub(super) const fn frame(&self) -> Part<T> {
        Part {
            total: self.total,
            items: Vec::new(),
            omitted: 0,
            more: None,
        }
    }
}

/// The items of a section that fit, out of `total`.
pub(crate) struct Part<T> {
    pub(crate) total: usize,
    pub(crate) items: Vec<T>,
    pub(crate) omitted: usize,
    pub(crate) more: Option<&'static str>,
}

impl<T: Output> Output for Part<T> {
    fn declare(fields: &mut Shown<'_, Self>) {
        fields.required("total", "The section's items.", &Count, |part| &part.total);
        let items = Items {
            item: Nested::<T>::new(),
        };
        fields.required("items", "The items that fit, in order.", &items, |part| {
            &part.items
        });
        fields.optional("omitted", "The items left out.", &Count, |part| {
            (part.omitted > 0).then_some(&part.omitted)
        });
        fields.optional("more", "Where the items left out are.", &Str, |part| {
            part.more
        });
    }
}
