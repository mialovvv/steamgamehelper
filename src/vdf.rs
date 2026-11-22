use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq)]
pub enum VdfValue {
    String(String),
    Object(BTreeMap<String, VdfValue>),
}

impl VdfValue {
    pub fn as_str(&self) -> Option<&str> {
        match self {
            VdfValue::String(s) => Some(s.as_str()),
            _ => None,
        }
    }

    pub fn as_object(&self) -> Option<&BTreeMap<String, VdfValue>> {
        match self {
            VdfValue::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn as_object_mut(&mut self) -> Option<&mut BTreeMap<String, VdfValue>> {
        match self {
            VdfValue::Object(map) => Some(map),
            _ => None,
        }
    }

    pub fn get(&self, key: &str) -> Option<&VdfValue> {
        match self {
            VdfValue::Object(map) => {
                // Case-insensitive key search
                map.iter()
                    .find(|(k, _)| k.eq_ignore_ascii_case(key))
                    .map(|(_, v)| v)
            }
            _ => None,
        }
    }

    pub fn get_str(&self, key: &str) -> Option<&str> {
        self.get(key).and_then(|v| v.as_str())
    }
}

pub fn parse_vdf(input: &str) -> Result<BTreeMap<String, VdfValue>, String> {
    let mut chars = input.chars().peekable();
    let mut root = BTreeMap::new();

    while let Some(key) = next_token(&mut chars)? {
        let value = parse_value(&mut chars)?;
        root.insert(key, value);
    }

    Ok(root)
}

fn skip_whitespace_and_comments<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    while let Some(&c) = chars.peek() {
        if c.is_whitespace() {
            chars.next();
        } else if c == '/' {
            chars.next();
            if chars.peek() == Some(&'/') {
                chars.next();
                // skip until newline
                while let Some(next_c) = chars.next() {
                    if next_c == '\n' {
                        break;
                    }
                }
            } else {
                // not a comment, push back logic or treat as syntax
                break;
            }
        } else {
            break;
        }
    }
}

fn next_token<I>(chars: &mut std::iter::Peekable<I>) -> Result<Option<String>, String>
where
    I: Iterator<Item = char>,
{
    skip_whitespace_and_comments(chars);

    let Some(&c) = chars.peek() else {
        return Ok(None);
    };

    if c == '{' || c == '}' {
        chars.next();
        return Ok(Some(c.to_string()));
    }

    if c == '"' {
        chars.next(); // consume opening quote
        let mut token = String::new();
        let mut escaped = false;
        while let Some(ch) = chars.next() {
            if escaped {
                match ch {
                    'n' => token.push('\n'),
                    't' => token.push('\t'),
                    'r' => token.push('\r'),
                    '\\' => token.push('\\'),
                    '"' => token.push('"'),
                    other => {
                        token.push('\\');
                        token.push(other);
                    }
                }
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                return Ok(Some(token));
            } else {
                token.push(ch);
            }
        }
        return Ok(Some(token));
    }

    // Bare token (unquoted)
    let mut token = String::new();
    while let Some(&ch) = chars.peek() {
        if ch.is_whitespace() || ch == '{' || ch == '}' || ch == '"' {
            break;
        }
        token.push(chars.next().unwrap());
    }

    if token.is_empty() {
        Ok(None)
    } else {
        Ok(Some(token))
    }
}

fn parse_value<I>(chars: &mut std::iter::Peekable<I>) -> Result<VdfValue, String>
where
    I: Iterator<Item = char>,
{
    let token = next_token(chars)?.ok_or_else(|| "Unexpected EOF reading value".to_string())?;
    if token == "{" {
        let mut map = BTreeMap::new();
        loop {
            let key = next_token(chars)?.ok_or_else(|| "Unexpected EOF in object".to_string())?;
            if key == "}" {
                break;
            }
            let val = parse_value(chars)?;
            map.insert(key, val);
        }
        Ok(VdfValue::Object(map))
    } else {
        Ok(VdfValue::String(token))
    }
}

pub fn serialize_vdf(map: &BTreeMap<String, VdfValue>, indent: usize) -> String {
    let mut out = String::new();
    let indent_str = "\t".repeat(indent);

    for (key, val) in map {
        match val {
            VdfValue::String(s) => {
                out.push_str(&format!("{}\"{}\"\t\t\"{}\"\n", indent_str, key, escape_str(s)));
            }
            VdfValue::Object(sub) => {
                out.push_str(&format!("{}\"{}\"\n{}{{\n", indent_str, key, indent_str));
                out.push_str(&serialize_vdf(sub, indent + 1));
                out.push_str(&format!("{}}}\n", indent_str));
            }
        }
    }
    out
}

fn escape_str(s: &str) -> String {
    s.replace('\\', "\\\\").replace('"', "\\\"")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_vdf_roundtrip() {
        let text = r#"
"users"
{
	"76561198710882680"
	{
		"AccountName"		"sbage75515"
		"PersonaName"		"ar202846"
		"RememberPassword"		"1"
		"WantsOfflineMode"		"1"
		"SkipOfflineModeWarning"		"0"
		"AutoLogin"		"1"
		"timestamp"		"1795349470"
	}
}
"#;
        let parsed = parse_vdf(text).expect("parse failed");
        let users = parsed.get("users").expect("users not found");
        let user = users.get("76561198710882680").expect("user not found");
        assert_eq!(user.get_str("AccountName"), Some("sbage75515"));
        assert_eq!(user.get_str("WantsOfflineMode"), Some("1"));
    }
}
