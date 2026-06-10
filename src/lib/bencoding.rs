use std::{collections::BTreeMap, range};

use crate::lib::bencoding::Token::{DictBegin, ListBegin, Number};

/// Representation of a value that can be bencoded
#[derive(Debug)]
#[derive(PartialEq)]
pub enum Bencodable {
    Number (i32),
    String(String),
    List(Vec<Bencodable>),
    Dict(BTreeMap<String, Bencodable>)
}

#[derive(PartialEq)]
#[derive(Debug)]
pub enum Token {
    /// `l`
    ListBegin,
    /// `d`
    DictBegin,
    /// `e`
    End,
    /// any number
    Number(i32),
    String(String)
}

#[derive(Debug)]
pub enum ParsingError {
    InvalidInput
}
pub type TokenizeResult = Result<Token, ParsingError>;

fn tokenize_int(chars: &Vec<char>, index: &mut usize) -> TokenizeResult {
    if chars[*index] == 'i' {
        *index += 1;
    }
    let res = tokenize_general_number(chars, index);
    if chars[*index] == 'e' {
        *index += 1;
    } else {
        return Err(ParsingError::InvalidInput);
    }
    res.map(|n| Token::Number(n))
}

fn tokenize_general_number(chars: &Vec<char>, index: &mut usize) -> Result<i32, ParsingError> {
    if !(chars[*index].is_ascii_digit() || chars[*index] == '-') {
        return Err(ParsingError::InvalidInput);
    }
    let mut to_parse = String::new();
    
    while chars[*index].is_ascii_digit() {
        to_parse.push(chars[*index]);
        *index += 1;
    }
    match to_parse.parse() {
        Ok(num) => Ok(num),
        _ => Err(ParsingError::InvalidInput)
    }
}

fn tokenize_string(chars: &Vec<char>, index: &mut usize) -> TokenizeResult {
    if !chars[*index].is_ascii_digit() {
        return Err(ParsingError::InvalidInput);
    }
    let length: usize = tokenize_general_number(chars, index)?.try_into().map_err(|_| ParsingError::InvalidInput)?;
    if chars[*index] == ':' {
        *index += 1;
    } else {
        return Err(ParsingError::InvalidInput);
    }
    let s: String = chars[*index..(*index + length)].iter().collect();
    *index += length;

    return Ok(Token::String(s));
}

fn tokenize_next(chars: &Vec<char>, index: &mut usize) -> TokenizeResult {
    let c = chars[*index];
    match c {
        'l' => {
            *index += 1;
            Ok(Token::ListBegin)
        }
        'd' => {
            *index += 1;
            Ok(Token::DictBegin)
        }
        'e' => {
            *index += 1;
            Ok(Token::End)
        }
        'i' => {
            tokenize_int(chars, index)
        }
        _ => {
            if c.is_ascii_digit() {
                return tokenize_string(chars, index);
            }
            Err(ParsingError::InvalidInput)
        }
    }
}

fn tokenize(encoded: String) -> Result<Vec<Token>, ParsingError> {
    let chars: Vec<char> = encoded.chars().collect();
    let mut idx: usize = 0;
    let mut tokens = vec![];
    while idx < chars.len() {
        tokens.push(tokenize_next(&chars, &mut idx)?);
    }
    Ok(tokens)
}

fn parse(tokens: &Vec<Token>, index: &mut usize) -> Result<Bencodable, ParsingError> {
    match &tokens[*index] {
        Token::Number(num) => Ok(Bencodable::Number(*num)),
        Token::String(s) => Ok(Bencodable::String(s.clone())),
        Token::DictBegin => {
            *index += 1;
            let mut map: BTreeMap<String, Bencodable> = BTreeMap::new();
            while tokens[*index] != Token::End {
                let key = if let Token::String(s) = &tokens[*index] {
                    s.clone()
                } else {
                    return Err(ParsingError::InvalidInput);
                };
                *index += 1;
                let value = parse(tokens, index)?;
                *index += 1;
                map.insert(key, value);
            }
            *index += 1;
            Ok(Bencodable::Dict(map))
            
        }
        Token::ListBegin => {
            *index += 1;
            let mut list: Vec<Bencodable> = vec![];
            while *index < tokens.len() && tokens[*index] != Token::End {
                let value = parse(tokens, index)?;
                *index += 1;
                list.push(value);
            }
            *index += 1;
            Ok(Bencodable::List(list))
            
        }
        _ => Err(ParsingError::InvalidInput)
    }
}

impl Bencodable {
    pub fn encode(&self) -> String {
        match self {
            Bencodable::String(string) => {
                format!("{}:{}", string.len(), string)
            },
            Bencodable::Number(num) => {
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

    pub fn decode(encoded: String) -> Result<Bencodable, ParsingError> {
        let tokens = tokenize(encoded)?;
        let mut index: usize = 0;
        Ok(parse(&tokens, &mut index)?)
    }
}

#[cfg(test)]
mod tests {
use super::*;

    #[test]
    fn test_tokenize_int_pass() {
        let inp = "i23e";
        let mut idx = 0;
        let res = tokenize_int(&inp.chars().collect(), &mut idx);
        assert_eq!(res.is_ok(), true);
        assert_eq!(idx, 4);
        assert_eq!(res.unwrap(), Token::Number(23));
    }

    #[test]
    fn test_tokenize_str_pass() {
        let inp = "5:hellooo";
        let mut idx = 0;
        let res = tokenize_string(&inp.chars().collect(), &mut idx);
        assert_eq!(res.is_ok(), true);
        assert_eq!(res.unwrap(), Token::String("hello".to_owned()));
    }

    #[test]
    fn test_tokenize_empty() {
        let res = tokenize("".to_string());
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), vec![]);
    }

    #[test]
    fn test_tokenize_number() {
        let res = tokenize("i42e".to_string());
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), vec![Token::Number(42)]);
    }

    #[test]
    fn test_tokenize_string() {
        let res = tokenize("5:hello".to_string());
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), vec![Token::String("hello".to_owned())]);
    }

    #[test]
    fn test_tokenize_list() {
        let res = tokenize("li1ei2ee".to_string());
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            vec![
                Token::ListBegin,
                Token::Number(1),
                Token::Number(2),
                Token::End
            ]
        );
    }

    #[test]
    fn test_tokenize_dict() {
        let res = tokenize("d3:fooi42ee".to_string());
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            vec![
                Token::DictBegin,
                Token::String("foo".to_owned()),
                Token::Number(42),
                Token::End
            ]
        );
    }

    #[test]
    fn test_tokenize_nested_list() {
        let res = tokenize("lli1eeli2eee".to_string());
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            vec![
                Token::ListBegin,
                Token::ListBegin,
                Token::Number(1),
                Token::End,
                Token::ListBegin,
                Token::Number(2),
                Token::End,
                Token::End
            ]
        );
    }

    #[test]
    fn test_tokenize_invalid() {
        let res = tokenize("x".to_string());
        assert!(res.is_err());
    }

    #[test]
    fn test_parse_number() {
        let tokens = vec![Token::Number(42)];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), Bencodable::Number(42));
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_parse_string() {
        let tokens = vec![Token::String("hello".to_owned())];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), Bencodable::String("hello".to_owned()));
        assert_eq!(idx, 0);
    }

    #[test]
    fn test_parse_list() {
        let tokens = vec![
            Token::ListBegin,
            Token::Number(1),
            Token::Number(2),
            Token::End,
        ];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            Bencodable::List(vec![Bencodable::Number(1), Bencodable::Number(2)])
        );
    }

    #[test]
    fn test_parse_dict() {
        let tokens = vec![
            Token::DictBegin,
            Token::String("key".to_owned()),
            Token::Number(42),
            Token::End,
        ];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_ok());
        let mut expected = BTreeMap::new();
        expected.insert("key".to_owned(), Bencodable::Number(42));
        assert_eq!(res.unwrap(), Bencodable::Dict(expected));
    }

    #[test]
    fn test_parse_nested_list() {
        let tokens = vec![
            Token::ListBegin,
            Token::ListBegin,
            Token::Number(1),
            Token::End,
            Token::End,
        ];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            Bencodable::List(vec![Bencodable::List(vec![Bencodable::Number(1)])])
        );
    }

    #[test]
    fn test_parse_invalid_token() {
        let tokens = vec![Token::End];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_err());
    }
}