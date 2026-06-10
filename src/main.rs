mod lib;

use std::collections::BTreeMap;

use lib::bencoding::Bencodable;

fn main() {
    println!("Hello, world!");
    let data: Bencodable = Bencodable::Dict(BTreeMap::from([
        ("hello".to_owned(), Bencodable::String("world".to_owned())),
        ("foo".to_owned(), Bencodable::Int(42))
    ]));
    let encoded = data.encode();
    println!("{}", encoded);
}
