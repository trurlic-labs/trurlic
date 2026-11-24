macro_rules! solo {
    ($name:ident) => { pub enum $name { One } };
}

solo!(Alone);
numbered_id!(Visible(u8), noun = "loose");
