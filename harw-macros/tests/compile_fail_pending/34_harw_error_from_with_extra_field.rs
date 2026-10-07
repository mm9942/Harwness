use harw_macros::HarwError;

#[derive(Debug, HarwError)]
enum E {
    Read {
        path: String,
        #[from]
        source: std::io::Error,
    },
}

fn main() {}
