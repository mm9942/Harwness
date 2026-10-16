use harw_macros::HarwError;

#[derive(Debug, HarwError)]
enum E {
    Both {
        #[source]
        a: std::io::Error,
        #[source]
        b: std::io::Error,
    },
}

fn main() {}
