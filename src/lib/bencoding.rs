use std::collections::BTreeMap;

use bstr::BString;

/// Representation of a value that can be bencoded
#[derive(PartialEq, Debug)]
pub enum Bencodable {
    Number(i64),
    String(BString),
    List(Vec<Bencodable>),
    Dict(BTreeMap<String, Bencodable>),
}

#[derive(PartialEq, Debug)]
pub enum Token {
    /// `l`
    ListBegin,
    /// `d`
    DictBegin,
    /// `e`
    End,
    /// any number
    Number(i64),
    String(BString),
}

#[derive(Debug)]
pub enum ParsingError {
    InvalidInput
}
pub type TokenizeResult = Result<Token, ParsingError>;

fn tokenize_int(chars: &Vec<u8>, index: &mut usize) -> TokenizeResult {
    if chars[*index] == b'i' {
        *index += 1;
    }
    let res = tokenize_general_number(chars, index);
    if chars[*index] == b'e' {
        *index += 1;
    } else {
        return Err(ParsingError::InvalidInput);
    }
    res.map(|n| Token::Number(n))
}

fn tokenize_general_number(chars: &Vec<u8>, index: &mut usize) -> Result<i64, ParsingError> {
    if !(chars[*index].is_ascii_digit() || chars[*index] == b'-') {
        return Err(ParsingError::InvalidInput);
    }
    let mut to_parse = BString::new(vec![]);
    if chars[*index] == b'-' {
        to_parse.push(b'-');
        *index += 1;
        if *index >= chars.len() || !chars[*index].is_ascii_digit() {
            return Err(ParsingError::InvalidInput);
        }
    }
    while *index < chars.len() && chars[*index].is_ascii_digit() {
        to_parse.push(chars[*index]);
        *index += 1;
    }
    to_parse
        .to_string()
        .parse()
        .map_err(|_| ParsingError::InvalidInput)
}

fn tokenize_string(chars: &Vec<u8>, index: &mut usize) -> TokenizeResult {
    if !chars[*index].is_ascii_digit() {
        return Err(ParsingError::InvalidInput);
    }
    let length: usize = tokenize_general_number(chars, index)?.try_into().map_err(|_| ParsingError::InvalidInput)?;
    if chars[*index] == ':' as u8 {
        *index += 1;
    } else {
        return Err(ParsingError::InvalidInput);
    }
    let s = BString::from(chars[*index..(*index + length)].iter().map(|x| x.clone()).collect::<Vec<u8>>());
    *index += length;

    return Ok(Token::String(s));
}

const L: u8 = 'l' as u8;
const D: u8 = 'd' as u8;
const E: u8 = 'e' as u8;
const I: u8 = 'i' as u8;

fn tokenize_next(chars: &Vec<u8>, index: &mut usize) -> TokenizeResult {
    let c = chars[*index];
    match c {
        L => {
            *index += 1;
            Ok(Token::ListBegin)
        }
        D => {
            *index += 1;
            Ok(Token::DictBegin)
        }
        E => {
            *index += 1;
            Ok(Token::End)
        }
        I => {
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

fn tokenize(chars: Vec<u8>) -> Result<Vec<Token>, ParsingError> {
    let mut idx: usize = 0;
    let mut tokens = vec![];
    while idx < chars.len() {
        tokens.push(tokenize_next(&chars, &mut idx)?);
    }
    Ok(tokens)
}

fn parse(tokens: &Vec<Token>, index: &mut usize) -> Result<Bencodable, ParsingError> {
    match &tokens[*index] {
        Token::Number(num) => {
            *index += 1;
            Ok(Bencodable::Number(*num))
        },
        Token::String(s) => {
            *index +=1 ;
            Ok(Bencodable::String(s.clone()))
        },
        Token::DictBegin => {
            *index += 1;
            let mut map: BTreeMap<String, Bencodable> = BTreeMap::new();
            while tokens[*index] != Token::End {
                let key = if let Token::String(s) = &tokens[*index] {
                    *index += 1;
                    s.to_string()
                } else {
                    return Err(ParsingError::InvalidInput);
                };
                let value = parse(tokens, index)?;
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
                //*index += 1;
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
                let elements = dict
                    .iter()
                    .map(|(k, v)| format!("{}:{}{}", k.len(), k, v.encode()))
                    .fold(String::new(), |a, b| a + &b);
                format!("d{}e", elements)
            }
        }
    }

    pub fn decode(encoded: Vec<u8>) -> Result<Bencodable, ParsingError> {
        let tokens = tokenize(encoded)?;
        let mut index: usize = 0;
        Ok(parse(&tokens, &mut index)?)
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Bencodable::Number(n) => Some(*n),
            _ => None,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Bencodable::String(s) => Some(s.as_ref()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> Option<String> {
        match self {
            Bencodable::String(s) => std::str::from_utf8(s.as_ref())
                .ok()
                .map(|s| s.to_owned()),
            _ => None,
        }
    }

    pub fn as_list(&self) -> Option<&Vec<Bencodable>> {
        match self {
            Bencodable::List(list) => Some(list),
            _ => None,
        }
    }

    pub fn as_dict(&self) -> Option<&BTreeMap<String, Bencodable>> {
        match self {
            Bencodable::Dict(dict) => Some(dict),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&Bencodable> {
        self.as_dict()?.get(key)
    }
}

#[cfg(test)]
mod tests {
use super::*;

    #[test]
    fn test_tokenize_int_pass() {
        let inp = "i23e";
        let mut idx = 0;
        let res = tokenize_int(&inp.bytes().collect(), &mut idx);
        assert_eq!(res.is_ok(), true);
        assert_eq!(idx, 4);
        assert_eq!(res.unwrap(), Token::Number(23));
    }

    #[test]
    fn test_tokenize_str_pass() {
        let inp = "5:hellooo";
        let mut idx = 0;
        let res = tokenize_string(&inp.bytes().collect(), &mut idx);
        assert_eq!(res.is_ok(), true);
        assert_eq!(res.unwrap(), Token::String("hello".into()));
    }

    #[test]
    fn test_tokenize_empty() {
        let res = tokenize("".into());
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), vec![]);
    }

    #[test]
    fn test_tokenize_number() {
        let res = tokenize("i42e".into());
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), vec![Token::Number(42)]);
    }

    #[test]
    fn test_tokenize_string() {
        let res = tokenize("5:hello".into());
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), vec![Token::String("hello".into())]);
    }

    #[test]
    fn test_tokenize_list() {
        let res = tokenize("li1ei2ee".into());
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
        let res = tokenize("d3:fooi42ee".into());
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            vec![
                Token::DictBegin,
                Token::String(BString::from("foo")),
                Token::Number(42),
                Token::End
            ]
        );
    }

    #[test]
    fn test_tokenize_nested_list() {
        let res = tokenize(BString::from("lli1eeli2eee").to_vec());
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
    fn test_tokenize_nested_list_2() {
        let res = tokenize(BString::from("ll40:udp://tracker.leechers-paradise.org:6969el34:udp://tracker.coppersurfer.tk:6969el33:udp://tracker.opentrackr.org:1337el23:udp://explodie.org:6969el31:udp://tracker.empire-js.us:1337el26:wss://tracker.btorrent.xyzel32:wss://tracker.openwebtorrent.comel25:wss://tracker.fastcast.nzee").to_vec());
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            vec![
                Token::ListBegin,
                Token::ListBegin,
                Token::String("udp://tracker.leechers-paradise.org:6969".into()),
                Token::End,
                Token::ListBegin,
                Token::String("udp://tracker.coppersurfer.tk:6969".into()),
                Token::End,
                Token::ListBegin,
                Token::String("udp://tracker.opentrackr.org:1337".into()),
                Token::End,
                Token::ListBegin,
                Token::String("udp://explodie.org:6969".into()),
                Token::End,
                Token::ListBegin,
                Token::String("udp://tracker.empire-js.us:1337".into()),
                Token::End,
                Token::ListBegin,
                Token::String("wss://tracker.btorrent.xyz".into()),
                Token::End,
                Token::ListBegin,
                Token::String("wss://tracker.openwebtorrent.com".into()),
                Token::End,
                Token::ListBegin,
                Token::String("wss://tracker.fastcast.nz".into()),
                Token::End,
                Token::End
            ]
        );
    }

    #[test]
    fn test_tokenize_invalid() {
        let res = tokenize(BString::from("x").to_vec());
        assert!(res.is_err());
    }

    #[test]
    fn test_parse_number() {
        let tokens = vec![Token::Number(42)];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), Bencodable::Number(42));
        assert_eq!(idx, 1);
    }

    #[test]
    fn test_parse_string() {
        let tokens = vec![Token::String("hello".into())];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_ok());
        assert_eq!(res.unwrap(), Bencodable::String("hello".into()));
        assert_eq!(idx, 1);
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
    fn test_parse_list_2() {
        let tokens = vec![
                Token::ListBegin,
                Token::ListBegin,
                Token::String("udp://tracker.leechers-paradise.org:6969".into()),
                Token::End,
                Token::ListBegin,
                Token::String("udp://tracker.coppersurfer.tk:6969".into()),
                Token::End,
                Token::ListBegin,
                Token::String("udp://tracker.opentrackr.org:1337".into()),
                Token::End,
                Token::ListBegin,
                Token::String("udp://explodie.org:6969".into()),
                Token::End,
                Token::ListBegin,
                Token::String("udp://tracker.empire-js.us:1337".into()),
                Token::End,
                Token::ListBegin,
                Token::String("wss://tracker.btorrent.xyz".into()),
                Token::End,
                Token::ListBegin,
                Token::String("wss://tracker.openwebtorrent.com".into()),
                Token::End,
                Token::ListBegin,
                Token::String("wss://tracker.fastcast.nz".into()),
                Token::End,
                Token::End
            ];
        let mut idx = 0;
        let res = parse(&tokens, &mut idx);
        assert!(res.is_ok());
        assert_eq!(
            res.unwrap(),
            Bencodable::List(vec![Bencodable::List(vec![Bencodable::String("udp://tracker.leechers-paradise.org:6969".into())]), Bencodable::List(vec![Bencodable::String("udp://tracker.coppersurfer.tk:6969".into())]), Bencodable::List(vec![Bencodable::String("udp://tracker.opentrackr.org:1337".into())]), Bencodable::List(vec![Bencodable::String("udp://explodie.org:6969".into())]), Bencodable::List(vec![Bencodable::String("udp://tracker.empire-js.us:1337".into())]), Bencodable::List(vec![Bencodable::String("wss://tracker.btorrent.xyz".into())]), Bencodable::List(vec![Bencodable::String("wss://tracker.openwebtorrent.com".into())]), Bencodable::List(vec![Bencodable::String("wss://tracker.fastcast.nz".into())])])
        );
    }

    #[test]
    fn test_parse_dict() {
        let tokens = vec![
            Token::DictBegin,
            Token::String("key".into()),
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