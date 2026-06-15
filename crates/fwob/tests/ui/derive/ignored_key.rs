use fwob_core::FwobFrame;

#[derive(FwobFrame)]
struct IgnoredKey {
    #[fwob(key, ignore)]
    key: i32,
}

fn main() {}
