//! TinyExpr's syntax over FORMAT.md's globals (^ is a power), evaluated for one frame-rate target.
use crate::Refused;

#[derive(Clone, Copy, Debug)]
pub enum Num {
    I(i128),
    F(f64),
}

impl Num {
    pub fn f(&self) -> f64 {
        match self {
            Num::I(i) => *i as f64,
            Num::F(f) => *f,
        }
    }
    pub fn trunc(&self) -> i128 {
        match self {
            Num::I(i) => *i,
            Num::F(f) => f.trunc() as i128,
        }
    }
}

fn global(name: &str, t: i64) -> Option<Num> {
    Some(match name {
        "FPS_TARGET" => Num::I(t as i128),
        "FRAMETIME_TARGET" => Num::F(1000.0 / t as f64),
        "VSYNC_TARGET" => Num::I(((60 / t).max(1)) as i128),
        "FPS_LOCK_TARGET" => Num::I(if t == 60 { t + 2 } else { t } as i128),
        _ => return None,
    })
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
    t: i64,
    src: &'a str,
}

fn bad<T>(src: &str, why: &str) -> Result<T, Refused> {
    Err(Refused(format!("expression {src:?}: {why}")))
}

pub fn eval(src: &str, t: i64) -> Result<Num, Refused> {
    let mut p = P { s: src.as_bytes(), i: 0, t, src };
    let v = p.sum()?;
    p.ws();
    if p.i != p.s.len() {
        return bad(src, "does not parse");
    }
    Ok(v)
}

fn arith(a: Num, b: Num, ii: impl Fn(i128, i128) -> Option<i128>, ff: impl Fn(f64, f64) -> f64) -> Num {
    match (a, b) {
        (Num::I(x), Num::I(y)) => ii(x, y).map(Num::I).unwrap_or_else(|| Num::F(ff(x as f64, y as f64))),
        _ => Num::F(ff(a.f(), b.f())),
    }
}

impl<'a> P<'a> {
    fn ws(&mut self) {
        while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
            self.i += 1;
        }
    }
    fn eat(&mut self, c: u8) -> bool {
        self.ws();
        if self.i < self.s.len() && self.s[self.i] == c {
            self.i += 1;
            true
        } else {
            false
        }
    }
    fn sum(&mut self) -> Result<Num, Refused> {
        let mut a = self.term()?;
        loop {
            if self.eat(b'+') {
                let b = self.term()?;
                a = arith(a, b, |x, y| x.checked_add(y), |x, y| x + y);
            } else if self.eat(b'-') {
                let b = self.term()?;
                a = arith(a, b, |x, y| x.checked_sub(y), |x, y| x - y);
            } else {
                return Ok(a);
            }
        }
    }
    fn term(&mut self) -> Result<Num, Refused> {
        let mut a = self.unary()?;
        loop {
            if self.eat(b'*') {
                let b = self.unary()?;
                a = arith(a, b, |x, y| x.checked_mul(y), |x, y| x * y);
            } else if self.eat(b'/') {
                let b = self.unary()?;
                if b.f() == 0.0 {
                    return bad(self.src, "divides by zero");
                }
                a = Num::F(a.f() / b.f());
            } else if self.eat(b'%') {
                let b = self.unary()?;
                if b.f() == 0.0 {
                    return bad(self.src, "divides by zero");
                }
                a = Num::F(a.f() % b.f());
            } else {
                return Ok(a);
            }
        }
    }
    // as Python reads it: a power binds tighter than a unary minus, and is right-associative
    fn unary(&mut self) -> Result<Num, Refused> {
        if self.eat(b'-') {
            let v = self.unary()?;
            return Ok(match v {
                Num::I(i) => Num::I(-i),
                Num::F(f) => Num::F(-f),
            });
        }
        if self.eat(b'+') {
            return self.unary();
        }
        self.power()
    }
    fn power(&mut self) -> Result<Num, Refused> {
        let a = self.atom()?;
        if self.eat(b'^') {
            let b = self.unary()?;
            return Ok(match (a, b) {
                (Num::I(x), Num::I(y)) if (0..64).contains(&y) => x.checked_pow(y as u32).map(Num::I).unwrap_or(Num::F((x as f64).powf(y as f64))),
                _ => Num::F(a.f().powf(b.f())),
            });
        }
        Ok(a)
    }
    fn atom(&mut self) -> Result<Num, Refused> {
        self.ws();
        let start = self.i;
        if self.eat(b'(') {
            let v = self.sum()?;
            return if self.eat(b')') { Ok(v) } else { bad(self.src, "has an unclosed (") };
        }
        let c = *self.s.get(self.i).ok_or_else(|| Refused(format!("expression {:?} ends early", self.src)))?;
        if c.is_ascii_digit() || c == b'.' {
            let mut j = self.i;
            while j < self.s.len() && (self.s[j].is_ascii_digit() || self.s[j] == b'.') {
                j += 1;
            }
            let txt = &self.src[self.i..j];
            self.i = j;
            return if txt.contains('.') { txt.parse().map(Num::F).or_else(|_| bad(self.src, "has a bad number")) } else { txt.parse().map(Num::I).or_else(|_| bad(self.src, "has a bad number")) };
        }
        if c.is_ascii_alphabetic() || c == b'_' {
            let mut j = self.i;
            while j < self.s.len() && (self.s[j].is_ascii_alphanumeric() || self.s[j] == b'_') {
                j += 1;
            }
            let name = &self.src[start..j];
            self.i = j;
            if self.eat(b'(') {
                let mut args = vec![self.sum()?];
                while self.eat(b',') {
                    args.push(self.sum()?);
                }
                if !self.eat(b')') {
                    return bad(self.src, "has an unclosed (");
                }
                let a = |k: usize| args.get(k).map(|n| n.f());
                let r = match (name, args.len()) {
                    ("TruncDec", 2) => {
                        let m = 10f64.powi(a(1).unwrap() as i32);
                        Num::F((a(0).unwrap() * m).trunc() / m)
                    }
                    ("floor", 1) => Num::F(a(0).unwrap().floor()),
                    ("ceil", 1) => Num::F(a(0).unwrap().ceil()),
                    ("abs", 1) => Num::F(a(0).unwrap().abs()),
                    ("sqrt", 1) => Num::F(a(0).unwrap().sqrt()),
                    ("pow", 2) => Num::F(a(0).unwrap().powf(a(1).unwrap())),
                    ("fmod", 2) => Num::F(a(0).unwrap() % a(1).unwrap()),
                    ("log", 1) => Num::F(a(0).unwrap().ln()),
                    ("log10", 1) => Num::F(a(0).unwrap().log10()),
                    ("exp", 1) => Num::F(a(0).unwrap().exp()),
                    _ => return bad(self.src, &format!("{name} is not supported")),
                };
                return Ok(r);
            }
            return global(name, self.t).ok_or_else(|| Refused(format!("expression {:?}: {name} is not supported", self.src)));
        }
        bad(self.src, "does not parse")
    }
}
