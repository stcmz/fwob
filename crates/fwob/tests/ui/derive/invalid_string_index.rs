use fwob_core::FwobFrame;

#[derive(FwobFrame)]
struct InvalidStringIndex {
    #[fwob(key)]
    key: i32,
    #[fwob(string_index)]
    symbol: u32,
}

fn main() {}
