use harw_macros::HarwError;

#[derive(Debug, HarwError)]
enum E {
    Bad {
        #[source]
        source: String,
    },
}

fn main() {}
