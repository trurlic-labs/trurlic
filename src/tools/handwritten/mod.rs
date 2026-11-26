//! The tools whose listing and arguments are written by hand, without a
//! spec: [`tool`] holds their schemas, parses their arguments and
//! dispatches, and [`context`] assembles what they answer.

mod context;
mod tool;

pub(super) use tool::Handwritten;
