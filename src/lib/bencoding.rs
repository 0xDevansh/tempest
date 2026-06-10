use std::collections::BTreeMap;

pub enum Bencodable {
    Int (i32),
    String(String),
    List(Vec<Bencodable>),
    Dict(BTreeMap<String, Bencodable>)
}

pub fn encode(data: &Bencodable) -> String {
    match data {
        Bencodable::String(string) => {
            format!("{}:{}", string.len(), string)
        },
        Bencodable::Int(num) => {
            format!("i{}e", num)
        },
        Bencodable::List(list) => {
            let elements = list.iter().map(|x| encode(x)).fold(String::new(), |a, b| a + &b);
            format!("l{}e", elements)
        },
        Bencodable::Dict(dict) => {
            let elements = dict.iter().map(|(k, v)| format!("{}{}", k, encode(v))).fold(String::new(), |a, b| a + &b);
            format!("d{}e", elements)
        }
    }
}