use fwob_core::FwobFrame;

#[derive(FwobFrame)]
struct InvalidTimestamp {
    #[fwob(key, timestamp = "minutes")]
    key: i64,
}

fn main() {}
