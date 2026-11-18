//! The YAML subset FPSLocker's configs use: block mappings and sequences by indentation, flow lists, comments, quoted strings, numbers (decimal, 0x hex, floats).
//! Anything else is an error, and an error refuses the config.

#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    Null,
    Bool(bool),
    Int(i64),
    Float(f64),
    Str(String),
    List(Vec<Value>),
    Map(Vec<(String, Value)>),
}

struct Line {
    indent: usize,
    text: String,
}

fn strip_comment(l: &str) -> String {
    let mut q: Option<char> = None;
    let mut prev = ' ';
    let mut out = String::new();
    for c in l.chars() {
        match q {
            Some(k) => {
                if c == k {
                    q = None;
                }
            }
            None => {
                if c == '"' || c == '\'' {
                    q = Some(c);
                } else if c == '#' && prev.is_whitespace() {
                    break;
                }
            }
        }
        out.push(c);
        prev = c;
    }
    out.trim_end().to_string()
}

pub fn parse(text: &str) -> Result<Value, String> {
    let mut lines = vec![];
    for raw in text.trim_start_matches('\u{feff}').lines() {
        if raw.contains('\t') && raw.trim_start_matches(' ').starts_with('\t') {
            return Err("the config does not parse: a tab indents a line".into());
        }
        let s = strip_comment(raw);
        if s.trim().is_empty() || s.trim() == "---" {
            continue;
        }
        let indent = s.len() - s.trim_start().len();
        lines.push(Line { indent, text: s.trim_start().to_string() });
    }
    if lines.is_empty() {
        return Ok(Value::Null);
    }
    let mut i = 0;
    let first = lines[0].indent;
    let v = block(&mut lines, &mut i, first)?;
    if i != lines.len() {
        return Err(format!("the config does not parse: line {:?}", lines[i].text));
    }
    Ok(v)
}

fn block(l: &mut Vec<Line>, i: &mut usize, indent: usize) -> Result<Value, String> {
    if l[*i].text == "-" || l[*i].text.starts_with("- ") {
        let mut out = vec![];
        while *i < l.len() && l[*i].indent == indent && (l[*i].text == "-" || l[*i].text.starts_with("- ")) {
            let rest = l[*i].text[1..].trim_start().to_string();
            if rest.is_empty() {
                *i += 1;
                if *i < l.len() && l[*i].indent > indent {
                    let ni = l[*i].indent;
                    out.push(block(l, i, ni)?);
                } else {
                    out.push(Value::Null);
                }
            } else if looks_like_key(&rest) {
                // "- key: value": a mapping whose first entry sits on the dash's line; its other entries are indented to the key's column
                let col = indent + (l[*i].text.len() - rest.len());
                l[*i] = Line { indent: col, text: rest };
                out.push(block(l, i, col)?);
            } else {
                *i += 1;
                out.push(scalar_or_flow(&rest)?);
            }
        }
        return Ok(Value::List(out));
    }
    if looks_like_key(&l[*i].text) {
        let mut out: Vec<(String, Value)> = vec![];
        while *i < l.len() && l[*i].indent == indent && looks_like_key(&l[*i].text) {
            let (k, rest) = split_key(&l[*i].text)?;
            *i += 1;
            let v = if rest.is_empty() {
                if *i < l.len() && (l[*i].indent > indent || (l[*i].indent == indent && (l[*i].text == "-" || l[*i].text.starts_with("- ")))) {
                    let ni = l[*i].indent;
                    block(l, i, ni)?
                } else {
                    Value::Null
                }
            } else {
                scalar_or_flow(&rest)?
            };
            out.push((k, v));
        }
        return Ok(Value::Map(out));
    }
    // a lone scalar
    let t = l[*i].text.clone();
    *i += 1;
    scalar_or_flow(&t)
}

fn looks_like_key(s: &str) -> bool {
    if s.starts_with('[') || s.starts_with('{') || s.starts_with('"') || s.starts_with('\'') {
        return false;
    }
    match s.find(':') {
        Some(p) => p > 0 && (s.len() == p + 1 || s[p + 1..].starts_with(' ')),
        None => false,
    }
}

fn split_key(s: &str) -> Result<(String, String), String> {
    let p = s.find(':').ok_or("not a mapping line")?;
    Ok((s[..p].trim().to_string(), s[p + 1..].trim().to_string()))
}

fn scalar_or_flow(s: &str) -> Result<Value, String> {
    let s = s.trim();
    if s.starts_with('[') {
        let (v, n) = flow(s, 0)?;
        if s[n..].trim().is_empty() {
            return Ok(v);
        }
        return Err(format!("the config does not parse: text after a list in {s:?}"));
    }
    if s.starts_with('{') {
        return Err("the config uses a flow mapping, which is not supported".into());
    }
    scalar(s)
}

fn flow(s: &str, mut i: usize) -> Result<(Value, usize), String> {
    let b = s.as_bytes();
    debug_assert_eq!(b[i], b'[');
    i += 1;
    let mut out = vec![];
    loop {
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i >= b.len() {
            return Err("the config does not parse: a list is not closed on its line".into());
        }
        if b[i] == b']' {
            return Ok((Value::List(out), i + 1));
        }
        if b[i] == b'[' {
            let (v, n) = flow(s, i)?;
            out.push(v);
            i = n;
        } else if b[i] == b'"' || b[i] == b'\'' {
            let q = b[i];
            let st = i + 1;
            i = st;
            while i < b.len() && b[i] != q {
                i += 1;
            }
            if i >= b.len() {
                return Err("the config does not parse: an unclosed quote".into());
            }
            out.push(Value::Str(s[st..i].to_string()));
            i += 1;
        } else {
            let st = i;
            while i < b.len() && b[i] != b',' && b[i] != b']' {
                i += 1;
            }
            out.push(scalar(s[st..i].trim())?);
        }
        while i < b.len() && (b[i] as char).is_whitespace() {
            i += 1;
        }
        if i < b.len() && b[i] == b',' {
            i += 1;
        }
    }
}

fn scalar(s: &str) -> Result<Value, String> {
    if (s.starts_with('"') && s.ends_with('"') && s.len() >= 2) || (s.starts_with('\'') && s.ends_with('\'') && s.len() >= 2) {
        return Ok(Value::Str(s[1..s.len() - 1].to_string()));
    }
    match s {
        "" | "~" | "null" | "Null" | "NULL" => return Ok(Value::Null),
        "true" | "True" | "TRUE" => return Ok(Value::Bool(true)),
        "false" | "False" | "FALSE" => return Ok(Value::Bool(false)),
        _ => {}
    }
    let (neg, body) = match s.strip_prefix('-') {
        Some(r) => (true, r),
        None => (false, s.strip_prefix('+').unwrap_or(s)),
    };
    if let Some(h) = body.strip_prefix("0x").or_else(|| body.strip_prefix("0X")) {
        if let Ok(n) = i64::from_str_radix(h, 16) {
            return Ok(Value::Int(if neg { -n } else { n }));
        }
        if let Ok(n) = u64::from_str_radix(h, 16) {
            return Ok(Value::Int(n as i64)); // beyond i64: kept as its bit pattern
        }
    }
    if !body.is_empty() && body.bytes().all(|c| c.is_ascii_digit()) {
        if let Ok(n) = body.parse::<i64>() {
            return Ok(Value::Int(if neg { -n } else { n }));
        }
    }
    if body.bytes().next().map_or(false, |c| c.is_ascii_digit() || c == b'.') && body.bytes().all(|c| c.is_ascii_digit() || matches!(c, b'.' | b'e' | b'E' | b'-' | b'+')) {
        if let Ok(f) = body.parse::<f64>() {
            return Ok(Value::Float(if neg { -f } else { f }));
        }
    }
    Ok(Value::Str(s.to_string()))
}
