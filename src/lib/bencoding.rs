use std::collections::BTreeMap;

/// Representation of a value that can be bencoded
#[derive(Debug)]
pub enum Bencodable {
    Int (i32),
    String(String),
    List(Vec<Bencodable>),
    Dict(BTreeMap<String, Bencodable>)
}

pub enum Token {
    /// `i`
    IntBegin,
    /// `l`
    ListBegin,
    /// `d`
    DictBegin,
    /// `e`
    End,
    /// `:`
    Colon,
    /// any number
    Number(i32),
}

fn make_token(chars: &Vec<char>, index: &mut usize) -> Token {
    
}

impl Bencodable {
    pub fn encode(&self) -> String {
        match self {
            Bencodable::String(string) => {
                format!("{}:{}", string.len(), string)
            },
            Bencodable::Int(num) => {
                format!("i{}e", num)
            },
            Bencodable::List(list) => {
                let elements = list.iter().map(|x| x.encode()).fold(String::new(), |a, b| a + &b);
                format!("l{}e", elements)
            },
            Bencodable::Dict(dict) => {
                let elements = dict.iter().map(|(k, v)| format!("{}{}", k, v.encode())).fold(String::new(), |a, b| a + &b);
                format!("d{}e", elements)
            }
        }
    }
}