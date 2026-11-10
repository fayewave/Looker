//! Valve's KeyValues text, the syntax of Source materials (.vmt) and Hammer maps (.vmf): `key value` pairs and
//! `key { ... }` blocks, keys and values quoted or bare, `//` comments. Keys repeat (a map's many `solid`s), so a
//! block is a list, in file order.

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Text(String),
    Block(Vec<(String, Value)>),
}

impl Value {
    pub fn text(&self) -> Option<&str> {
        match self {
            Value::Text(t) => Some(t),
            Value::Block(_) => None,
        }
    }

    pub fn block(&self) -> &[(String, Value)] {
        match self {
            Value::Block(b) => b,
            Value::Text(_) => &[],
        }
    }
}

/// The first text value of `key` (case ignored) in `block`.
pub fn get<'a>(block: &'a [(String, Value)], key: &str) -> Option<&'a str> {
    block.iter().find(|(k, _)| k.eq_ignore_ascii_case(key)).and_then(|(_, v)| v.text())
}

/// Every block named `key` (case ignored) in `block`.
pub fn blocks<'a>(block: &'a [(String, Value)], key: &'a str) -> impl Iterator<Item = &'a [(String, Value)]> + 'a {
    block.iter().filter(move |(k, v)| k.eq_ignore_ascii_case(key) && matches!(v, Value::Block(_))).map(|(_, v)| v.block())
}

#[derive(Debug, PartialEq)]
enum Token {
    Word(String),
    Open,
    Close,
}

fn tokens(text: &str) -> Vec<Token> {
    let mut out = Vec::new();
    let mut it = text.chars().peekable();
    while let Some(&c) = it.peek() {
        match c {
            '{' => {
                it.next();
                out.push(Token::Open);
            }
            '}' => {
                it.next();
                out.push(Token::Close);
            }
            '"' => {
                it.next();
                let mut s = String::new();
                for c in it.by_ref() {
                    if c == '"' {
                        break;
                    }
                    s.push(c);
                }
                out.push(Token::Word(s));
            }
            '/' if {
                let mut look = it.clone();
                look.next();
                look.peek() == Some(&'/')
            } =>
            {
                while it.next().is_some_and(|c| c != '\n') {}
            }
            c if c.is_whitespace() => {
                it.next();
            }
            _ => {
                let mut s = String::new();
                while let Some(&c) = it.peek() {
                    if c.is_whitespace() || c == '{' || c == '}' || c == '"' {
                        break;
                    }
                    s.push(c);
                    it.next();
                }
                out.push(Token::Word(s));
            }
        }
    }
    out
}

/// The whole text as one top-level block. Forgiving: a missing `}` closes at the end, a stray one is skipped,
/// a key with nothing after it is dropped.
pub fn parse(text: &str) -> Vec<(String, Value)> {
    let toks = tokens(text.trim_start_matches('\u{FEFF}'));
    let mut i = 0;
    let mut top = Vec::new();
    while i < toks.len() {
        top.extend(block(&toks, &mut i));
        if i < toks.len() && toks[i] == Token::Close {
            i += 1;
        }
    }
    top
}

fn block(toks: &[Token], i: &mut usize) -> Vec<(String, Value)> {
    let mut out = Vec::new();
    while *i < toks.len() {
        match &toks[*i] {
            Token::Close => return out,
            Token::Open => {
                // A block without a key: keep its contents under an empty one.
                *i += 1;
                let b = block(toks, i);
                *i += 1;
                out.push((String::new(), Value::Block(b)));
            }
            Token::Word(k) => {
                *i += 1;
                match toks.get(*i) {
                    Some(Token::Word(v)) => {
                        out.push((k.clone(), Value::Text(v.clone())));
                        *i += 1;
                    }
                    Some(Token::Open) => {
                        *i += 1;
                        let b = block(toks, i);
                        *i += 1;
                        out.push((k.clone(), Value::Block(b)));
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_bare_nested_and_comments() {
        let kv = parse("\"LightmappedGeneric\"\n{\n  // a comment\n  $basetexture \"brick/wall01\" // trailing\n  \"$surfaceprop\" brick\n  Proxies { AnimatedTexture { animatedtexturevar $basetexture } }\n}\n");
        assert_eq!(kv.len(), 1);
        assert_eq!(kv[0].0, "LightmappedGeneric");
        let b = kv[0].1.block();
        assert_eq!(get(b, "$BaseTexture"), Some("brick/wall01"));
        assert_eq!(get(b, "$surfaceprop"), Some("brick"));
        assert_eq!(blocks(b, "proxies").count(), 1);
    }

    #[test]
    fn repeated_keys_and_a_missing_brace() {
        let kv = parse("world { solid { id 1 } solid { id 2 }");
        let w = kv[0].1.block();
        let ids: Vec<_> = blocks(w, "solid").map(|s| get(s, "id").unwrap()).collect();
        assert_eq!(ids, ["1", "2"]);
    }

    #[test]
    fn slashes_in_values_are_not_comments() {
        let kv = parse("x { $basetexture models/props/crate }");
        assert_eq!(get(kv[0].1.block(), "$basetexture"), Some("models/props/crate"));
    }
}
