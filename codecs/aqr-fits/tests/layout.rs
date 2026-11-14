//! Layouts read from a formula's parameters are checked, whatever the numbers are.

use aqr_fits::rice::Params;
use aqr_fits::tiled::Layout;

fn layout(field: u64) -> Layout {
    // 4 pixels in one tile; a row of 8 bytes (a `P` descriptor) in a table of one row
    Layout {
        rice: Params { bytepix: 2, block: 32 },
        dims: vec![4],
        tile: vec![4],
        row: 8,
        rows: 1,
        field,
        q: false,
        heap: 8,
        len: 8,
    }
}

#[test]
fn field_past_the_row_is_invalid() {
    assert_eq!(Layout::from_bytes(&layout(0).to_bytes(), &mut 0), Some(layout(0)));
    // (the field and its descriptor, summed, wrap around to a place inside the row)
    for field in [1, 8, u64::MAX - 3, u64::MAX] {
        assert_eq!(Layout::from_bytes(&layout(field).to_bytes(), &mut 0), None, "field {field}");
    }
}
