ids::numbered_id! { ShelfId(u16), noun = "shelf" }
crate_only_id! { Hidden }
create_exception!(shelf, ShelfFull, PyException, "The shelf is full.");
